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
        int cancelAuthenticatedPlan(int generation, long plan);
    }

    /** Resolves Android-owned values into non-zero native-registry handles. */
    public interface NativeHandleResolver {
        NativeHandles resolve(NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws NativePlanException;
    }

    /** Six separately typed opaque IDs; no Java object, string, or byte buffer crosses JNI. */
    public static final class NativeHandles {
        private final long endpoint;
        private final long username;
        private final long knownHost;
        private final long credentialRef;
        private final long publicKey;

        public NativeHandles(long endpoint, long username, long knownHost, long credentialRef, long publicKey) {
            if (endpoint <= 0 || username <= 0 || knownHost <= 0 || credentialRef <= 0 || publicKey <= 0) {
                throw new IllegalArgumentException("native handles must be positive");
            }
            this.endpoint = endpoint;
            this.username = username;
            this.knownHost = knownHost;
            this.credentialRef = credentialRef;
            this.publicKey = publicKey;
        }

        @Override public String toString() { return "NativeHandles(REDACTED)"; }
    }

    /** Factory for a bounded native plan token. It does not open a socket or credential. */
    public static final class PlanFactory {
        private static final int ABI_VERSION = 1;
        private final NativePlanBridge bridge;
        private final NativeHandleResolver handles;

        public PlanFactory(NativePlanBridge bridge, NativeHandleResolver handles) {
            this.bridge = Objects.requireNonNull(bridge, "bridge");
            this.handles = Objects.requireNonNull(handles, "handles");
        }

        public Plan begin(int generation, NativeAuthenticatedSshConnector.NativeConnectionInput input)
            throws NativePlanException {
            if (generation <= 0 || bridge.abiVersion() != ABI_VERSION) throw new NativePlanException();
            long plan = bridge.beginAuthenticatedPlan(generation, handles.resolve(Objects.requireNonNull(input, "input")));
            if (plan <= 0) throw new NativePlanException();
            return new Plan(bridge, generation, plan);
        }
    }

    /** One-close-only opaque native plan token. It has no authenticated operations. */
    public static final class Plan implements AutoCloseable {
        private final NativePlanBridge bridge;
        private final int generation;
        private long value;

        private Plan(NativePlanBridge bridge, int generation, long value) {
            this.bridge = bridge;
            this.generation = generation;
            this.value = value;
        }

        @Override public void close() throws NativePlanException {
            if (value == 0) return;
            long closing = value;
            value = 0;
            if (bridge.cancelAuthenticatedPlan(generation, closing) != 0) throw new NativePlanException();
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
                handles.credentialRef, handles.publicKey
            );
        }

        @Override public int cancelAuthenticatedPlan(int generation, long plan) {
            return nativeCancelAuthenticatedPlan(generation, plan);
        }

        private static native int nativeAbiVersion();
        private static native long nativeBeginAuthenticatedPlan(
            int generation, long endpoint, long username, long knownHost, long credentialRef, long publicKey
        );
        private static native int nativeCancelAuthenticatedPlan(int generation, long plan);
    }
}
