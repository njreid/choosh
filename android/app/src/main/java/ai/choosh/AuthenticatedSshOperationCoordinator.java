package ai.choosh;

import java.util.Objects;

/**
 * Framework-free composition seam for opening operations on one verified SSH connection.
 *
 * <p>The Android composition root injects a profile lookup and a native SSH adapter. The
 * coordinator deliberately has no socket, private-key, URI, path, or JNI API. In particular,
 * a {@link VerifiedSshConnector} MUST verify the exact persisted host key before it asks its
 * credential adapter to use the opaque credential reference in {@link ConnectionRequest}.
 * Successful operations are represented only by the narrow capability supplied by that adapter.
 * This keeps Compose, Activities, and future JNI wiring outside the policy boundary and makes
 * the ordering and failure mapping deterministic in unit tests.</p>
 */
public final class AuthenticatedSshOperationCoordinator {
    private final ProfileConnectionSource profiles;
    private final VerifiedSshConnector connector;

    public AuthenticatedSshOperationCoordinator(
        ProfileConnectionSource profiles,
        VerifiedSshConnector connector
    ) {
        this.profiles = Objects.requireNonNull(profiles, "profiles");
        this.connector = Objects.requireNonNull(connector, "connector");
    }

    /** Opens one connection for an explicitly selected profile. */
    public void open(ProfileId profileId, OutcomeListener listener) {
        Objects.requireNonNull(profileId, "profileId");
        Objects.requireNonNull(listener, "listener");

        final ConnectionRequest request;
        try {
            request = profiles.connectionFor(profileId);
        } catch (ProfileUnavailableException exception) {
            listener.onComplete(OpenOutcome.failure(OpenCode.PROFILE_UNAVAILABLE));
            return;
        }
        if (request == null) {
            listener.onComplete(OpenOutcome.failure(OpenCode.PROFILE_UNAVAILABLE));
            return;
        }

        Completion completion = new Completion(listener);
        try {
            connector.openVerified(request, completion::completeOnce);
        } catch (SshTransportException exception) {
            completion.completeOnce(ConnectorResult.failure(ConnectorCode.TRANSPORT_UNAVAILABLE));
        }
    }

    /** Retrieves durable profile-owned connection metadata without opening a transport. */
    public interface ProfileConnectionSource {
        ConnectionRequest connectionFor(ProfileId profileId) throws ProfileUnavailableException;
    }

    /**
     * Outer adapter over the native SSH graph.
     *
     * <p>The adapter is responsible for exact host-key admission before using the credential
     * reference, public-key authentication, and all Rust transport limits. It must not expose a
     * pre-authenticated session to Android callers.</p>
     */
    public interface VerifiedSshConnector {
        void openVerified(ConnectionRequest request, ConnectorCallback callback) throws SshTransportException;
    }

    public interface ConnectorCallback {
        void onResult(ConnectorResult result);
    }

    public interface OutcomeListener {
        void onComplete(OpenOutcome outcome);
    }

    /** Opaque profile identifier; it is not a remote path, username, or hostname. */
    public static final class ProfileId {
        private static final int MAX_BYTES = 128;
        private final String value;

        public ProfileId(String value) {
            if (!isBoundedToken(value)) throw new IllegalArgumentException("invalid profile identifier");
            this.value = value;
        }

        public String valueForProfileStore() {
            return value;
        }

        @Override public String toString() {
            return "ProfileId(REDACTED)";
        }
    }

    /**
     * Immutable profile-owned material supplied to the verified native connector.
     * No private key is present; {@code credentialRef} is meaningful only to the Keystore adapter.
     */
    public static final class ConnectionRequest {
        private final ProfileId profileId;
        private final SshKeyImportCoordinator.OpaqueCredentialRef credentialRef;
        private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

        public ConnectionRequest(
            ProfileId profileId,
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey
        ) {
            this.profileId = Objects.requireNonNull(profileId, "profileId");
            this.credentialRef = Objects.requireNonNull(credentialRef, "credentialRef");
            this.publicKey = Objects.requireNonNull(publicKey, "publicKey");
        }

        public ProfileId profileId() { return profileId; }

        /** Intended only for the native Keystore-backed credential signer adapter. */
        public SshKeyImportCoordinator.OpaqueCredentialRef credentialRef() { return credentialRef; }

        public SshKeyImportCoordinator.PublicKeyMetadata publicKey() { return publicKey; }

        @Override public String toString() {
            return "ConnectionRequest(profile=" + profileId + ", credential=REDACTED, publicKey="
                + publicKey.algorithm() + ")";
        }
    }

    /** Narrow post-authentication capability; new channel kinds are added as separate ports. */
    public interface AuthenticatedOperations {
        void executeRpc(RpcRequest request, RpcCallback callback) throws SshTransportException;
    }

    /** Versioned, bounded RPC request bytes supplied to the native fixed-command adapter. */
    public static final class RpcRequest {
        private static final int MAX_REQUEST_BYTES = 1_048_576;
        private final byte[] bytes;

        public RpcRequest(byte[] bytes) {
            Objects.requireNonNull(bytes, "bytes");
            if (bytes.length == 0 || bytes.length > MAX_REQUEST_BYTES) {
                throw new IllegalArgumentException("invalid RPC request length");
            }
            this.bytes = bytes.clone();
        }

        public byte[] copyBytesForNativeAdapter() { return bytes.clone(); }
    }

    public interface RpcCallback {
        void onResult(RpcResult result);
    }

    /** Opaque, bounded RPC response bytes; protocol decoding belongs to the Rust boundary. */
    public static final class RpcResult {
        private static final int MAX_RESPONSE_BYTES = 1_048_576;
        private final byte[] bytes;

        public RpcResult(byte[] bytes) {
            Objects.requireNonNull(bytes, "bytes");
            if (bytes.length == 0 || bytes.length > MAX_RESPONSE_BYTES) {
                throw new IllegalArgumentException("invalid RPC response length");
            }
            this.bytes = bytes.clone();
        }

        public byte[] copyBytesForProtocolDecoder() { return bytes.clone(); }
    }

    public static final class ConnectorResult {
        private final ConnectorCode code;
        private final AuthenticatedOperations operations;

        private ConnectorResult(ConnectorCode code, AuthenticatedOperations operations) {
            this.code = code;
            this.operations = operations;
        }

        public static ConnectorResult connected(AuthenticatedOperations operations) {
            return new ConnectorResult(ConnectorCode.CONNECTED, Objects.requireNonNull(operations, "operations"));
        }

        public static ConnectorResult failure(ConnectorCode code) {
            if (code == ConnectorCode.CONNECTED) throw new IllegalArgumentException("failure requires a failure code");
            return new ConnectorResult(code, null);
        }
    }

    public static final class OpenOutcome {
        private final OpenCode code;
        private final AuthenticatedOperations operations;

        private OpenOutcome(OpenCode code, AuthenticatedOperations operations) {
            this.code = code;
            this.operations = operations;
        }

        public static OpenOutcome connected(AuthenticatedOperations operations) {
            return new OpenOutcome(OpenCode.CONNECTED, Objects.requireNonNull(operations, "operations"));
        }

        public static OpenOutcome failure(OpenCode code) {
            if (code == OpenCode.CONNECTED) throw new IllegalArgumentException("failure requires a failure code");
            return new OpenOutcome(code, null);
        }

        public OpenCode code() { return code; }
        public AuthenticatedOperations operations() { return operations; }
    }

    public enum ConnectorCode { CONNECTED, HOST_KEY_REJECTED, AUTHENTICATION_FAILED, TRANSPORT_UNAVAILABLE }

    public enum OpenCode { CONNECTED, PROFILE_UNAVAILABLE, HOST_KEY_REJECTED, AUTHENTICATION_FAILED, TRANSPORT_UNAVAILABLE }

    public static final class ProfileUnavailableException extends Exception {
        public ProfileUnavailableException() { super(); }
    }

    public static final class SshTransportException extends Exception {
        public SshTransportException() { super(); }
    }

    private static final class Completion {
        private final OutcomeListener listener;
        private boolean completed;

        Completion(OutcomeListener listener) { this.listener = listener; }

        void completeOnce(ConnectorResult result) {
            if (completed) return;
            completed = true;
            if (result == null) {
                listener.onComplete(OpenOutcome.failure(OpenCode.TRANSPORT_UNAVAILABLE));
                return;
            }
            switch (result.code) {
                case CONNECTED:
                    if (result.operations == null) {
                        listener.onComplete(OpenOutcome.failure(OpenCode.TRANSPORT_UNAVAILABLE));
                    } else {
                        listener.onComplete(OpenOutcome.connected(result.operations));
                    }
                    return;
                case HOST_KEY_REJECTED:
                    listener.onComplete(OpenOutcome.failure(OpenCode.HOST_KEY_REJECTED));
                    return;
                case AUTHENTICATION_FAILED:
                    listener.onComplete(OpenOutcome.failure(OpenCode.AUTHENTICATION_FAILED));
                    return;
                case TRANSPORT_UNAVAILABLE:
                    listener.onComplete(OpenOutcome.failure(OpenCode.TRANSPORT_UNAVAILABLE));
                    return;
                default:
                    listener.onComplete(OpenOutcome.failure(OpenCode.TRANSPORT_UNAVAILABLE));
            }
        }
    }

    private static boolean isBoundedToken(String value) {
        if (value == null || value.isEmpty() || value.length() > MAX_TOKEN_CHARS) return false;
        if (value.getBytes(java.nio.charset.StandardCharsets.UTF_8).length > ProfileId.MAX_BYTES) return false;
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            if (!(character >= 'a' && character <= 'z')
                && !(character >= 'A' && character <= 'Z')
                && !(character >= '0' && character <= '9')
                && character != '_' && character != '-') return false;
        }
        return true;
    }

    private static final int MAX_TOKEN_CHARS = 128;
}
