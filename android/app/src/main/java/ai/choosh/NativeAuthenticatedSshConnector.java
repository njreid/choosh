package ai.choosh;

import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Android outer adapter from the framework-free SSH coordinator to a native Rust connector.
 *
 * <p>The injected port is the sole JNI composition point.  It receives separately typed host,
 * user, exact host-key, public-key metadata, and an opaque credential reference; it never
 * receives private-key bytes, a shell command, a URI, or a filesystem path. The Rust side MUST
 * verify the host key before asking its Keystore signer to use that reference.</p>
 */
public final class NativeAuthenticatedSshConnector
    implements AuthenticatedSshOperationCoordinator.VerifiedSshConnector {
    private final NativeConnectorPort nativeConnector;

    public NativeAuthenticatedSshConnector(NativeConnectorPort nativeConnector) {
        this.nativeConnector = Objects.requireNonNull(nativeConnector, "nativeConnector");
    }

    @Override public void openVerified(
        AuthenticatedSshOperationCoordinator.ConnectionRequest request,
        AuthenticatedSshOperationCoordinator.ConnectorCallback callback
    ) throws AuthenticatedSshOperationCoordinator.SshTransportException {
        Objects.requireNonNull(request, "request");
        Objects.requireNonNull(callback, "callback");
        NativeOpenCompletion completion = new NativeOpenCompletion(callback);
        try {
            nativeConnector.open(new NativeConnectionInput(request), completion::completeOnce);
        } catch (NativeBridgeException exception) {
            throw new AuthenticatedSshOperationCoordinator.SshTransportException();
        }
    }

    /** Injectable JNI/Rust port. Its production implementation owns all native calls. */
    public interface NativeConnectorPort {
        void open(NativeConnectionInput input, NativeOpenCallback callback) throws NativeBridgeException;
    }

    public interface NativeOpenCallback {
        void onComplete(NativeOpenResult result);
    }

    /**
     * Typed, non-secret input passed once into the native composition root.
     *
     * <p>The opaque credential reference is usable only by the Android Keystore adapter. These
     * methods intentionally do not produce a combined connection string or diagnostic text.</p>
     */
    public static final class NativeConnectionInput {
        private final ProfileConnectionMetadataSource.SshEndpoint endpoint;
        private final ProfileConnectionMetadataSource.SshUsername username;
        private final ProfileConnectionMetadataSource.KnownHost knownHost;
        private final SshKeyImportCoordinator.OpaqueCredentialRef credentialRef;
        private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

        private NativeConnectionInput(AuthenticatedSshOperationCoordinator.ConnectionRequest request) {
            endpoint = request.endpoint();
            username = request.username();
            knownHost = request.knownHost();
            credentialRef = request.credentialRef();
            publicKey = request.publicKey();
        }

        public ProfileConnectionMetadataSource.SshEndpoint endpoint() { return endpoint; }
        public ProfileConnectionMetadataSource.SshUsername username() { return username; }
        public ProfileConnectionMetadataSource.KnownHost knownHost() { return knownHost; }
        public SshKeyImportCoordinator.OpaqueCredentialRef credentialRef() { return credentialRef; }
        public SshKeyImportCoordinator.PublicKeyMetadata publicKey() { return publicKey; }

        @Override public String toString() {
            return "NativeConnectionInput(endpoint=REDACTED, username=REDACTED, knownHost="
                + knownHost.algorithm() + ", credential=REDACTED, publicKey=" + publicKey.algorithm() + ")";
        }
    }

    /** Native open result; only a verified native session may accompany {@link Code#CONNECTED}. */
    public static final class NativeOpenResult {
        private final Code code;
        private final NativeSession session;

        private NativeOpenResult(Code code, NativeSession session) {
            this.code = code;
            this.session = session;
        }

        public static NativeOpenResult connected(NativeSession session) {
            return new NativeOpenResult(Code.CONNECTED, Objects.requireNonNull(session, "session"));
        }

        public static NativeOpenResult failure(Code code) {
            if (code == Code.CONNECTED) throw new IllegalArgumentException("failure requires a failure code");
            return new NativeOpenResult(Objects.requireNonNull(code, "code"), null);
        }
    }

    public enum Code { CONNECTED, HOST_KEY_REJECTED, AUTHENTICATION_FAILED, TRANSPORT_UNAVAILABLE }

    /** Opaque native session with only the existing bounded RPC capability. */
    public interface NativeSession {
        void executeRpc(byte[] framedRequest, NativeRpcCallback callback) throws NativeBridgeException;
    }

    public interface NativeRpcCallback {
        void onComplete(NativeRpcResult result);
    }

    /** Bounded bytes returned by the Rust protocol boundary, never a live native handle. */
    public static final class NativeRpcResult {
        private final byte[] response;

        public NativeRpcResult(byte[] response) {
            Objects.requireNonNull(response, "response");
            if (response.length == 0 || response.length > 1_048_576) {
                throw new IllegalArgumentException("invalid native RPC response length");
            }
            this.response = response.clone();
        }

        private AuthenticatedSshOperationCoordinator.RpcResult toRpcResult() {
            return new AuthenticatedSshOperationCoordinator.RpcResult(response);
        }
    }

    public static final class NativeBridgeException extends Exception {
        public NativeBridgeException() { super(); }
    }

    private static final class NativeOpenCompletion {
        private final AtomicBoolean complete = new AtomicBoolean();
        private final AuthenticatedSshOperationCoordinator.ConnectorCallback callback;

        NativeOpenCompletion(AuthenticatedSshOperationCoordinator.ConnectorCallback callback) {
            this.callback = callback;
        }

        void completeOnce(NativeOpenResult result) {
            if (!complete.compareAndSet(false, true)) return;
            if (result == null) {
                callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                    AuthenticatedSshOperationCoordinator.ConnectorCode.TRANSPORT_UNAVAILABLE
                ));
                return;
            }
            switch (result.code) {
                case CONNECTED:
                    if (result.session == null) {
                        callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                            AuthenticatedSshOperationCoordinator.ConnectorCode.TRANSPORT_UNAVAILABLE
                        ));
                    } else {
                        callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.connected(
                            new NativeOperations(result.session)
                        ));
                    }
                    return;
                case HOST_KEY_REJECTED:
                    callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                        AuthenticatedSshOperationCoordinator.ConnectorCode.HOST_KEY_REJECTED
                    ));
                    return;
                case AUTHENTICATION_FAILED:
                    callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                        AuthenticatedSshOperationCoordinator.ConnectorCode.AUTHENTICATION_FAILED
                    ));
                    return;
                case TRANSPORT_UNAVAILABLE:
                default:
                    callback.onResult(AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                        AuthenticatedSshOperationCoordinator.ConnectorCode.TRANSPORT_UNAVAILABLE
                    ));
            }
        }
    }

    private static final class NativeOperations
        implements AuthenticatedSshOperationCoordinator.AuthenticatedOperations {
        private final NativeSession session;

        NativeOperations(NativeSession session) { this.session = session; }

        @Override public void executeRpc(
            AuthenticatedSshOperationCoordinator.RpcRequest request,
            AuthenticatedSshOperationCoordinator.RpcCallback callback
        ) throws AuthenticatedSshOperationCoordinator.SshTransportException {
            Objects.requireNonNull(request, "request");
            Objects.requireNonNull(callback, "callback");
            try {
                session.executeRpc(request.copyBytesForNativeAdapter(), result -> {
                    if (result == null) throw new IllegalArgumentException("native RPC result missing");
                    callback.onResult(result.toRpcResult());
                });
            } catch (NativeBridgeException exception) {
                throw new AuthenticatedSshOperationCoordinator.SshTransportException();
            }
        }
    }
}
