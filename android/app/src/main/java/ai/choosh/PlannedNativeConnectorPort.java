package ai.choosh;

import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Composition-only adapter which admits an opaque Rust plan before opening a native connection.
 *
 * <p>The plan factory resolves Android-owned metadata into opaque IDs and validates the JNI ABI.
 * The injected transport is deliberately separate: it receives the plan token, never credential
 * bytes or a combined endpoint string. A plan is cancelled exactly once after the native open
 * completes (or throws), so it cannot outlive its requested connection.</p>
 */
public final class PlannedNativeConnectorPort
    implements NativeAuthenticatedSshConnector.NativeConnectorPort {
    private final ConnectionGeneration generations;
    private final RustNativeConnectorJni.PlanFactory plans;
    private final PlannedTransportPort transport;

    public PlannedNativeConnectorPort(
        ConnectionGeneration generations,
        RustNativeConnectorJni.PlanFactory plans,
        PlannedTransportPort transport
    ) {
        this.generations = Objects.requireNonNull(generations, "generations");
        this.plans = Objects.requireNonNull(plans, "plans");
        this.transport = Objects.requireNonNull(transport, "transport");
    }

    @Override public void open(
        NativeAuthenticatedSshConnector.NativeConnectionInput input,
        NativeAuthenticatedSshConnector.NativeOpenCallback callback
    ) throws NativeAuthenticatedSshConnector.NativeBridgeException {
        Objects.requireNonNull(input, "input");
        Objects.requireNonNull(callback, "callback");
        final RustNativeConnectorJni.Plan plan;
        try {
            plan = plans.begin(generations.next(), input);
        } catch (RustNativeConnectorJni.NativePlanException exception) {
            throw new NativeAuthenticatedSshConnector.NativeBridgeException();
        }

        Completion completion = new Completion(plan, callback);
        try {
            transport.open(plan, completion::complete);
        } catch (NativeAuthenticatedSshConnector.NativeBridgeException exception) {
            completion.cancel();
            throw exception;
        }
    }

    /** Injected monotonically positive generation source; no mutable global state. */
    public interface ConnectionGeneration {
        int next() throws NativeAuthenticatedSshConnector.NativeBridgeException;
    }

    /** Native transport continuation which can consume only the already-admitted plan. */
    public interface PlannedTransportPort {
        void open(
            RustNativeConnectorJni.Plan plan,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback
        ) throws NativeAuthenticatedSshConnector.NativeBridgeException;
    }

    /**
     * Production adapter from an opened native plan into its sole session owner.
     */
    public static final class JniPlannedTransport implements PlannedTransportPort {
        private static final int STATUS_OK = 0;

        @Override public void open(
            RustNativeConnectorJni.Plan plan,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback
        ) throws NativeAuthenticatedSshConnector.NativeBridgeException {
            Objects.requireNonNull(plan, "plan");
            Objects.requireNonNull(callback, "callback");
            try {
                if (plan.open() != STATUS_OK) {
                    throw new NativeAuthenticatedSshConnector.NativeBridgeException();
                }
                callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.connected(
                    new JniNativeSession(plan.transferToSession())
                ));
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                throw new NativeAuthenticatedSshConnector.NativeBridgeException();
            }
        }
    }

    /** Bounded Java facade over the sole native session lease. */
    static final class JniNativeSession implements NativeAuthenticatedSshConnector.NativeSession {
        private final RustNativeConnectorJni.SessionLease lease;
        private final AtomicBoolean closed = new AtomicBoolean();

        JniNativeSession(RustNativeConnectorJni.SessionLease lease) {
            this.lease = Objects.requireNonNull(lease, "lease");
        }

        @Override public void executeRpc(
            byte[] framedRequest, NativeAuthenticatedSshConnector.NativeRpcCallback callback
        ) throws NativeAuthenticatedSshConnector.NativeBridgeException {
            if (closed.get() || framedRequest == null || callback == null) {
                throw new NativeAuthenticatedSshConnector.NativeBridgeException();
            }
            try {
                callback.onComplete(new NativeAuthenticatedSshConnector.NativeRpcResult(
                    lease.executeRpc(framedRequest)
                ));
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                throw new NativeAuthenticatedSshConnector.NativeBridgeException();
            }
        }

        @Override public void close() throws NativeAuthenticatedSshConnector.NativeBridgeException {
            if (!closed.compareAndSet(false, true)) return;
            try {
                lease.close();
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                throw new NativeAuthenticatedSshConnector.NativeBridgeException();
            }
        }
    }

    private static final class Completion {
        private RustNativeConnectorJni.Plan plan;
        private NativeAuthenticatedSshConnector.NativeOpenCallback callback;
        private final AtomicBoolean complete = new AtomicBoolean();

        Completion(RustNativeConnectorJni.Plan plan, NativeAuthenticatedSshConnector.NativeOpenCallback callback) {
            this.plan = plan;
            this.callback = callback;
        }

        void complete(NativeAuthenticatedSshConnector.NativeOpenResult result) {
            if (!complete.compareAndSet(false, true)) return;
            NativeAuthenticatedSshConnector.NativeOpenCallback completion = takeCallback();
            // JniNativeSession transfers the plan before invoking this callback, making closePlan
            // a harmless no-op there. Other transports retain the original cancellation rule.
            if (!closePlan()) {
                completion.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                    NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE
                ));
                return;
            }
            completion.onComplete(result);
        }

        void cancel() {
            if (!complete.compareAndSet(false, true)) return;
            takeCallback();
            closePlan();
        }

        private NativeAuthenticatedSshConnector.NativeOpenCallback takeCallback() {
            NativeAuthenticatedSshConnector.NativeOpenCallback value = callback;
            callback = null;
            return value;
        }

        boolean closePlan() {
            RustNativeConnectorJni.Plan closing = plan;
            plan = null;
            if (closing == null) return true;
            try {
                closing.close();
                return true;
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                return false;
            }
        }
    }
}
