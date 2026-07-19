package ai.choosh;

import java.nio.charset.StandardCharsets;
import java.util.Objects;

/**
 * Converts one durable, non-secret SSH profile record into the input for an authenticated
 * connection attempt.
 *
 * <p>The outer composition root injects a durable {@link ProfileMetadataStore}. This source has
 * no Android framework dependency, socket, private-key material, URI, filesystem path, or
 * logging API. It carries the exact persisted host-key identity alongside separately encoded
 * endpoint and username fields so a {@link AuthenticatedSshOperationCoordinator.VerifiedSshConnector}
 * can verify the host before it asks a credential adapter to use the opaque credential reference.</p>
 */
public final class ProfileConnectionMetadataSource
    implements AuthenticatedSshOperationCoordinator.ProfileConnectionSource {
    private final ProfileMetadataStore profiles;

    public ProfileConnectionMetadataSource(ProfileMetadataStore profiles) {
        this.profiles = Objects.requireNonNull(profiles, "profiles");
    }

    @Override public AuthenticatedSshOperationCoordinator.ConnectionRequest connectionFor(
        AuthenticatedSshOperationCoordinator.ProfileId profileId
    ) throws AuthenticatedSshOperationCoordinator.ProfileUnavailableException {
        Objects.requireNonNull(profileId, "profileId");
        ProfileConnectionMetadata profile = profiles.load(profileId);
        if (profile == null || !profile.profileId().equals(profileId)) {
            throw new AuthenticatedSshOperationCoordinator.ProfileUnavailableException();
        }
        return profile.toConnectionRequest();
    }

    /** Durable-store port. Implementations own encryption and persistence outside this seam. */
    public interface ProfileMetadataStore {
        ProfileConnectionMetadata load(AuthenticatedSshOperationCoordinator.ProfileId profileId)
            throws AuthenticatedSshOperationCoordinator.ProfileUnavailableException;
    }

    /** Immutable non-secret connection metadata owned by exactly one profile. */
    public static final class ProfileConnectionMetadata {
        private final AuthenticatedSshOperationCoordinator.ProfileId profileId;
        private final SshEndpoint endpoint;
        private final SshUsername username;
        private final KnownHost knownHost;
        private final SshKeyImportCoordinator.OpaqueCredentialRef credentialRef;
        private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

        public ProfileConnectionMetadata(
            AuthenticatedSshOperationCoordinator.ProfileId profileId,
            SshEndpoint endpoint,
            SshUsername username,
            KnownHost knownHost,
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey
        ) {
            this.profileId = Objects.requireNonNull(profileId, "profileId");
            this.endpoint = Objects.requireNonNull(endpoint, "endpoint");
            this.username = Objects.requireNonNull(username, "username");
            this.knownHost = Objects.requireNonNull(knownHost, "knownHost");
            this.credentialRef = Objects.requireNonNull(credentialRef, "credentialRef");
            this.publicKey = Objects.requireNonNull(publicKey, "publicKey");
        }

        public AuthenticatedSshOperationCoordinator.ProfileId profileId() { return profileId; }

        AuthenticatedSshOperationCoordinator.ConnectionRequest toConnectionRequest() {
            return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
                profileId, endpoint, username, knownHost, credentialRef, publicKey
            );
        }
    }

    /** Host and port kept as separate values for the native connector; URLs are never accepted. */
    public static final class SshEndpoint {
        private final String host;
        private final int port;

        public SshEndpoint(String host, int port) {
            if (!isDnsOrIpv4Host(host)) throw new IllegalArgumentException("invalid SSH host");
            if (port < 1 || port > 65_535) throw new IllegalArgumentException("invalid SSH port");
            this.host = host;
            this.port = port;
        }

        /** Intended only for the native network adapter, never a shell or log argument. */
        public String hostForNativeConnector() { return host; }
        public int portForNativeConnector() { return port; }

        @Override public String toString() { return "SshEndpoint(REDACTED)"; }
    }

    /** SSH username encoded separately from endpoint and commands. */
    public static final class SshUsername {
        private final String value;

        public SshUsername(String value) {
            if (!isBoundedUsername(value)) throw new IllegalArgumentException("invalid SSH username");
            this.value = value;
        }

        /** Intended only for the native authentication adapter. */
        public String valueForNativeConnector() { return value; }

        @Override public String toString() { return "SshUsername(REDACTED)"; }
    }

    /** Exact, persisted host key identity; a presented mismatch is not eligible for first trust. */
    public static final class KnownHost {
        private final HostKeyAlgorithm algorithm;
        private final String sha256Fingerprint;

        public KnownHost(HostKeyAlgorithm algorithm, String sha256Fingerprint) {
            this.algorithm = Objects.requireNonNull(algorithm, "algorithm");
            if (!isCanonicalSha256Fingerprint(sha256Fingerprint)) {
                throw new IllegalArgumentException("invalid host-key fingerprint");
            }
            this.sha256Fingerprint = sha256Fingerprint;
        }

        public HostKeyAlgorithm algorithm() { return algorithm; }
        /** Intended only for the native exact-host-key verifier. */
        public String sha256FingerprintForVerifier() { return sha256Fingerprint; }

        @Override public String toString() {
            return "KnownHost(algorithm=" + algorithm + ", fingerprint=REDACTED)";
        }
    }

    public enum HostKeyAlgorithm { ED25519, ECDSA, RSA }

    private static boolean isDnsOrIpv4Host(String host) {
        if (host == null || host.isEmpty() || host.length() > 253
            || host.getBytes(StandardCharsets.UTF_8).length > 253
            || host.charAt(0) == '.' || host.charAt(host.length() - 1) == '.') return false;
        int labelLength = 0;
        for (int index = 0; index < host.length(); index++) {
            char character = host.charAt(index);
            if (character == '.') {
                if (labelLength == 0 || host.charAt(index - 1) == '-') return false;
                labelLength = 0;
                continue;
            }
            if (!(character >= 'a' && character <= 'z')
                && !(character >= 'A' && character <= 'Z')
                && !(character >= '0' && character <= '9') && character != '-') return false;
            if (labelLength == 0 && character == '-') return false;
            labelLength++;
            if (labelLength > 63) return false;
        }
        return labelLength > 0 && host.charAt(host.length() - 1) != '-';
    }

    private static boolean isBoundedUsername(String value) {
        if (value == null || value.isEmpty() || value.length() > 64
            || value.getBytes(StandardCharsets.UTF_8).length > 64) return false;
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            if (!(character >= 'a' && character <= 'z')
                && !(character >= 'A' && character <= 'Z')
                && !(character >= '0' && character <= '9')
                && character != '_' && character != '-' && character != '.') return false;
        }
        return true;
    }

    private static boolean isCanonicalSha256Fingerprint(String fingerprint) {
        if (fingerprint == null || !fingerprint.startsWith("SHA256:") || fingerprint.length() != 50) return false;
        for (int index = 7; index < fingerprint.length(); index++) {
            char character = fingerprint.charAt(index);
            if (!(character >= 'A' && character <= 'Z')
                && !(character >= 'a' && character <= 'z')
                && !(character >= '0' && character <= '9')
                && character != '+' && character != '/') return false;
        }
        return true;
    }
}
