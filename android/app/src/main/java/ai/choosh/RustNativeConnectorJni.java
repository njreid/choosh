package ai.choosh;

import java.util.Objects;

/**
 * First pointer-free JNI boundary for an authenticated SSH connection plan.
 *
 * <p>This class is intentionally <em>not</em> a {@link NativeAuthenticatedSshConnector}.
 * Creating an opaque native plan is not host-key verification or authentication, and therefore
 * cannot return connected operations. The later transport bridge must consume this plan only
 * after it has verified the exact host key and obtained a signature through the Keystore adapter.</p>
 */
public final class RustNativeConnectorJni {
    private RustNativeConnectorJni() { }

    /** JNI port kept injectable so JVM tests do not load native code. */
    public interface NativePlanBridge {
        int abiVersion();
        long beginAuthenticatedPlan(int generation, NativeHandles handles);
        int openAuthenticatedPlan(int generation, long plan);
        int cancelAuthenticatedPlan(int generation, long plan);
    }

    /** Resolves Android-owned values into non-zero native-registry handles. */
    public interface NativeHandleResolver {
        NativeHandles resolve(NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws NativePlanException;
    }

    /**
     * Android-owned runtime registry for one connection attempt.
     *
     * <p>The runtime may retain a socket adapter and Keystore challenge callback in its own
     * registry, but JNI receives only the resulting opaque IDs. Its returned lease defines the
     * sole release point for those registrations; it is injected rather than process-global.</p>
     */
    public interface NativeRuntime {
        NativeLease acquire(NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws NativePlanException;
    }

    /** One-close-only ownership lease for Android runtime registrations. */
    public static final class NativeLease implements AutoCloseable {
        private final NativeHandles handles;
        private final Releaser releaser;
        private boolean closed;

        public NativeLease(NativeHandles handles, Releaser releaser) {
            this.handles = Objects.requireNonNull(handles, "handles");
            this.releaser = Objects.requireNonNull(releaser, "releaser");
        }

        NativeHandles handles() { return handles; }

        @Override public synchronized void close() throws NativePlanException {
            if (closed) return;
            closed = true;
            releaser.release();
        }

        /** Releases Android-owned callback and stream registrations without exposing them. */
        public interface Releaser {
            void release() throws NativePlanException;
        }
    }

    /** Seven separately typed opaque IDs; no Java object, string, or byte buffer crosses JNI. */
    public static final class NativeHandles {
        private final long endpoint;
        private final long username;
        private final long knownHost;
        private final long credentialRef;
        private final long publicKey;
        private final long signingCallback;
        private final long runtimeLease;

        public NativeHandles(
            long endpoint, long username, long knownHost, long credentialRef, long publicKey,
            long signingCallback, long runtimeLease
        ) {
            if (endpoint <= 0 || username <= 0 || knownHost <= 0 || credentialRef <= 0 || publicKey <= 0
                || signingCallback <= 0 || runtimeLease <= 0) {
                throw new IllegalArgumentException("native handles must be positive");
            }
            this.endpoint = endpoint;
            this.username = username;
            this.knownHost = knownHost;
            this.credentialRef = credentialRef;
            this.publicKey = publicKey;
            this.signingCallback = signingCallback;
            this.runtimeLease = runtimeLease;
        }

        @Override public String toString() { return "NativeHandles(REDACTED)"; }
    }

    /** Factory for a bounded native plan token. It does not open a socket or credential. */
    public static final class PlanFactory {
        private static final int ABI_VERSION = 3;
        private final NativePlanBridge bridge;
        private final NativeRuntime runtime;

        /** Legacy adapter for stateless opaque-handle resolution. */
        public static PlanFactory fromHandleResolver(NativePlanBridge bridge, NativeHandleResolver handles) {
            return new PlanFactory(bridge, (NativeRuntime) input -> new NativeLease(
                Objects.requireNonNull(handles, "handles").resolve(input), () -> { }
            ));
        }

        /** Creates plans from an explicit Android runtime registry lifecycle. */
        public PlanFactory(NativePlanBridge bridge, NativeRuntime runtime) {
            this.bridge = Objects.requireNonNull(bridge, "bridge");
            this.runtime = Objects.requireNonNull(runtime, "runtime");
        }

        public Plan begin(int generation, NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws NativePlanException {
            if (generation <= 0 || bridge.abiVersion() != ABI_VERSION) throw new NativePlanException();
            NativeLease lease = runtime.acquire(Objects.requireNonNull(input, "input"));
            try {
                long plan = bridge.beginAuthenticatedPlan(generation, lease.handles());
                if (plan <= 0) throw new NativePlanException();
                return new Plan(bridge, generation, plan, lease);
            } catch (NativePlanException exception) {
                lease.close();
                throw exception;
            }
        }
    }

    /** One-close-only opaque native plan token. It has no authenticated operations. */
    public static final class Plan implements AutoCloseable {
        private final NativePlanBridge bridge;
        private final int generation;
        private final NativeLease lease;
        private long value;

        private Plan(NativePlanBridge bridge, int generation, long value, NativeLease lease) {
            this.bridge = bridge;
            this.generation = generation;
            this.value = value;
            this.lease = lease;
        }

        /**
         * Advances this admitted token through the native transport boundary.
         *
         * <p>The status is intentionally opaque to callers: only the production transport
         * adapter maps it to a public connection result. The token remains owned by this
         * lifecycle and must still be closed exactly once.</p>
         */
        int open() throws NativePlanException {
            if (value == 0) throw new NativePlanException();
            return bridge.openAuthenticatedPlan(generation, value);
        }

        @Override public void close() throws NativePlanException {
            if (value == 0) return;
            long closing = value;
            value = 0;
            NativePlanException failure = null;
            if (bridge.cancelAuthenticatedPlan(generation, closing) != 0) failure = new NativePlanException();
            try {
                lease.close();
            } catch (NativePlanException releaseFailure) {
                if (failure == null) failure = releaseFailure;
            }
            if (failure != null) throw failure;
        }

        @Override public String toString() { return "NativeAuthenticatedPlan(REDACTED)"; }
    }

    public static final class NativePlanException extends Exception {
        public NativePlanException() { super(); }
    }

    /** Production JNI implementation. Loading happens only when this adapter is selected. */
    public static final class JniPlanBridge implements NativePlanBridge {
        static { System.loadLibrary("choosh_android_bridge"); }

        @Override public int abiVersion() { return nativeAbiVersion(); }

        @Override public long beginAuthenticatedPlan(int generation, NativeHandles handles) {
            return nativeBeginAuthenticatedPlan(
                generation, handles.endpoint, handles.username, handles.knownHost,
                handles.credentialRef, handles.publicKey, handles.signingCallback, handles.runtimeLease
            );
        }

        @Override public int openAuthenticatedPlan(int generation, long plan) {
            return nativeOpenAuthenticatedPlan(generation, plan);
        }

        @Override public int cancelAuthenticatedPlan(int generation, long plan) {
            return nativeCancelAuthenticatedPlan(generation, plan);
        }

        private static native int nativeAbiVersion();
        private static native long nativeBeginAuthenticatedPlan(
            int generation, long endpoint, long username, long knownHost, long credentialRef, long publicKey,
            long signingCallback, long runtimeLease
        );
        private static native int nativeCancelAuthenticatedPlan(int generation, long plan);
        private static native int nativeOpenAuthenticatedPlan(int generation, long plan);
    }
}
