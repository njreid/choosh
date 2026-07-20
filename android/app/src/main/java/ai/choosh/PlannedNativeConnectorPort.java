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
            completion.closePlan();
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
     * First production transport adapter. The current native bridge is intentionally fail-closed
     * while its handle registry cannot yet compose an injected stream, exact host-key verifier,
     * and Keystore signer. This adapter therefore never reports a connected session from plan
     * admission alone.
     */
    public static final class JniPlannedTransport implements PlannedTransportPort {
        private static final int STATUS_TRANSPORT_UNAVAILABLE = 5;

        @Override public void open(
            RustNativeConnectorJni.Plan plan,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback
        ) throws NativeAuthenticatedSshConnector.NativeBridgeException {
            Objects.requireNonNull(plan, "plan");
            Objects.requireNonNull(callback, "callback");
            try {
                if (plan.open() != STATUS_TRANSPORT_UNAVAILABLE) {
                    throw new NativeAuthenticatedSshConnector.NativeBridgeException();
                }
                callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                    NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE));
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                throw new NativeAuthenticatedSshConnector.NativeBridgeException();
            }
        }
    }

    private static final class Completion {
        private final RustNativeConnectorJni.Plan plan;
        private final NativeAuthenticatedSshConnector.NativeOpenCallback callback;
        private final AtomicBoolean complete = new AtomicBoolean();

        Completion(RustNativeConnectorJni.Plan plan, NativeAuthenticatedSshConnector.NativeOpenCallback callback) {
            this.plan = plan;
            this.callback = callback;
        }

        void complete(NativeAuthenticatedSshConnector.NativeOpenResult result) {
            if (!complete.compareAndSet(false, true)) return;
            if (!closePlan()) {
                callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                    NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE
                ));
                return;
            }
            callback.onComplete(result);
        }

        boolean closePlan() {
            try {
                plan.close();
                return true;
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                return false;
            }
        }
    }
}
