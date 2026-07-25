package ai.choosh;

import java.util.Arrays;
import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Constructor-injected Android owner for one bounded native SSH runtime lease.
 *
 * <p>The lease binds an already-opened Android socket, validated non-secret identity metadata,
 * one public key, and a payload-only signer callback. Rust receives only the narrow callback
 * object and opaque handles. This class has no static registry: the returned lease owns the
 * callback and closes the socket exactly once.</p>
 */
public final class BoundedAndroidNativeRuntime implements RustNativeConnectorJni.NativeRuntime {
    private static final int MAX_OPERATION_BYTES = 65_536;
    private static final int MAX_PUBLIC_KEY_BYTES = 8 * 1024;

    private final RustNativeConnectorJni.NativeHandleResolver handles;
    private final BoundedAndroidSocketAdapter sockets;
    private final PublicKeySource publicKeys;
    private final LeaseSignerSource signers;

    public BoundedAndroidNativeRuntime(
        RustNativeConnectorJni.NativeHandleResolver handles,
        BoundedAndroidSocketAdapter sockets,
        PublicKeySource publicKeys,
        LeaseSignerSource signers
    ) {
        this.handles = Objects.requireNonNull(handles, "handles");
        this.sockets = Objects.requireNonNull(sockets, "sockets");
        this.publicKeys = Objects.requireNonNull(publicKeys, "publicKeys");
        this.signers = Objects.requireNonNull(signers, "signers");
    }

    @Override public RustNativeConnectorJni.NativeLease acquire(
        NativeAuthenticatedSshConnector.NativeConnectionInput input
    ) throws RustNativeConnectorJni.NativePlanException {
        Objects.requireNonNull(input, "input");
        final RustNativeConnectorJni.NativeHandles resolved = handles.resolve(input);
        final BoundedAndroidSocketAdapter.Connection socket;
        try {
            socket = sockets.open(input.endpoint());
        } catch (BoundedAndroidSocketAdapter.SocketOpenException | RuntimeException exception) {
            throw new RustNativeConnectorJni.NativePlanException();
        }
        try {
            byte[] publicKey = copyPublicKey(publicKeys.publicKey(input));
            LeaseSigner signer = Objects.requireNonNull(signers.bind(input), "signer");
            byte[] metadata = AndroidRuntimeMetadata.encode(input.username(), input.knownHost(), input.publicKey());
            RuntimeCallbacks callbacks = new RuntimeCallbacks(
                resolved, socket, metadata, publicKey, signer
            );
            return new RustNativeConnectorJni.NativeLease(
                resolved, callbacks, () -> releaseCallbacks(callbacks)
            );
        } catch (RustNativeConnectorJni.NativePlanException exception) {
            closeAfterRejection(socket);
            throw exception;
        } catch (RuntimeException exception) {
            closeAfterRejection(socket);
            throw new RustNativeConnectorJni.NativePlanException();
        }
    }

    private static byte[] copyPublicKey(byte[] value) {
        Objects.requireNonNull(value, "public key");
        if (value.length == 0 || value.length > MAX_PUBLIC_KEY_BYTES) {
            throw new IllegalArgumentException("invalid public key length");
        }
        return value.clone();
    }

    private static void closeAfterRejection(BoundedAndroidSocketAdapter.Connection socket) {
        try {
            socket.close();
        } catch (BoundedAndroidSocketAdapter.SocketIoException ignored) {
            // The acquisition failure remains authoritative and content-free.
        }
    }

    private static void releaseCallbacks(RuntimeCallbacks callbacks)
        throws RustNativeConnectorJni.NativePlanException {
        try {
            callbacks.close();
        } catch (AndroidRuntimeCallbackPort.CallbackException exception) {
            throw new RustNativeConnectorJni.NativePlanException();
        }
    }

    /** Android-owned source of the non-secret canonical OpenSSH public identity. */
    public interface PublicKeySource {
        byte[] publicKey(NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws RustNativeConnectorJni.NativePlanException;
    }

    /** Android-owned factory which binds one opaque credential to a payload-only signer. */
    public interface LeaseSignerSource {
        LeaseSigner bind(NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws RustNativeConnectorJni.NativePlanException;
    }

    /** Payload-only signer; it does not receive a credential selector or public-key selector. */
    public interface LeaseSigner {
        byte[] sign(byte[] payload) throws RustNativeConnectorJni.NativePlanException;
    }

    private static final class RuntimeCallbacks implements AndroidRuntimeCallbackPort, AutoCloseable {
        private final RustNativeConnectorJni.NativeHandles handles;
        private final BoundedAndroidSocketAdapter.Connection socket;
        private final byte[] metadata;
        private final byte[] publicKey;
        private final LeaseSigner signer;
        private final AtomicBoolean closed = new AtomicBoolean();

        RuntimeCallbacks(
            RustNativeConnectorJni.NativeHandles handles,
            BoundedAndroidSocketAdapter.Connection socket,
            byte[] metadata,
            byte[] publicKey,
            LeaseSigner signer
        ) {
            this.handles = Objects.requireNonNull(handles, "handles");
            this.socket = Objects.requireNonNull(socket, "socket");
            this.metadata = Objects.requireNonNull(metadata, "metadata").clone();
            this.publicKey = Objects.requireNonNull(publicKey, "publicKey").clone();
            this.signer = Objects.requireNonNull(signer, "signer");
        }

        @Override public byte[] metadata(long lease) throws CallbackException {
            ensureLease(lease);
            return metadata.clone();
        }

        @Override public byte[] publicKey(long lease) throws CallbackException {
            ensureLease(lease);
            return publicKey.clone();
        }

        @Override public byte[] read(long lease, int maximumBytes) throws CallbackException {
            ensureLease(lease);
            if (maximumBytes <= 0 || maximumBytes > MAX_OPERATION_BYTES) throw new CallbackException();
            byte[] bytes = new byte[maximumBytes];
            try {
                int count = socket.read(bytes, 0, bytes.length);
                return count < 0 ? new byte[0] : Arrays.copyOf(bytes, count);
            } catch (BoundedAndroidSocketAdapter.SocketIoException | RuntimeException exception) {
                throw new CallbackException();
            }
        }

        @Override public void write(long lease, byte[] bytes) throws CallbackException {
            ensureLease(lease);
            if (bytes == null || bytes.length == 0 || bytes.length > MAX_OPERATION_BYTES) {
                throw new CallbackException();
            }
            try {
                socket.write(bytes.clone(), 0, bytes.length);
            } catch (BoundedAndroidSocketAdapter.SocketIoException | RuntimeException exception) {
                throw new CallbackException();
            }
        }

        @Override public byte[] sign(long lease, byte[] payload) throws CallbackException {
            ensureLease(lease);
            if (payload == null || payload.length == 0 || payload.length > MAX_OPERATION_BYTES) {
                throw new CallbackException();
            }
            try {
                byte[] signature = signer.sign(payload.clone());
                if (signature == null || signature.length == 0 || signature.length > MAX_OPERATION_BYTES) {
                    throw new CallbackException();
                }
                return signature.clone();
            } catch (RustNativeConnectorJni.NativePlanException | RuntimeException exception) {
                throw new CallbackException();
            }
        }

        @Override public void close(long lease) throws CallbackException {
            ensureLease(lease);
            close();
        }

        @Override public void close() throws CallbackException {
            if (!closed.compareAndSet(false, true)) return;
            try {
                socket.close();
            } catch (BoundedAndroidSocketAdapter.SocketIoException | RuntimeException exception) {
                throw new CallbackException();
            }
        }

        private void ensureLease(long lease) throws CallbackException {
            if (closed.get() || lease != handles.runtimeLease()) throw new CallbackException();
        }
    }
}
