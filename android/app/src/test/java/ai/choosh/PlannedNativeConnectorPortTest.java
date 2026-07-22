package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

/** Headless lifecycle checks for the plan-to-native-transport composition seam. */
public final class PlannedNativeConnectorPortTest {
    @Test public void admits_opaque_plan_before_transport_and_cancels_on_completion() throws Exception {
        RecordingBridge bridge = new RecordingBridge(3, 29, 0);
        RecordingTransport transport = new RecordingTransport();
        PlannedNativeConnectorPort port = port(bridge, transport);
        Outcome outcome = new Outcome();

        port.open(input(), outcome);

        assertEquals(7, bridge.generation);
        assertEquals("NativeAuthenticatedPlan(REDACTED)", transport.plan.toString());
        transport.callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.HOST_KEY_REJECTED
        ));
        transport.callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.AUTHENTICATION_FAILED
        ));
        assertEquals(1, bridge.cancels);
        assertEquals(1, outcome.completions);
    }

    @Test public void plan_rejection_never_invokes_transport() {
        RecordingTransport transport = new RecordingTransport();
        PlannedNativeConnectorPort port = port(new RecordingBridge(3, 0, 0), transport);
        assertThrows(NativeAuthenticatedSshConnector.NativeBridgeException.class,
            () -> port.open(input(), result -> { }));
        assertEquals(0, transport.opens);
    }

    @Test public void transport_throw_cancels_plan_before_propagating() {
        RecordingBridge bridge = new RecordingBridge(3, 29, 0);
        RecordingTransport transport = new RecordingTransport();
        PlannedNativeConnectorPort port = port(bridge, (plan, callback) -> {
            transport.callback = callback;
            throw new NativeAuthenticatedSshConnector.NativeBridgeException();
        });
        Outcome outcome = new Outcome();
        assertThrows(NativeAuthenticatedSshConnector.NativeBridgeException.class,
            () -> port.open(input(), outcome));
        assertEquals(1, bridge.cancels);
        transport.callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.AUTHENTICATION_FAILED
        ));
        assertEquals(0, outcome.completions);
    }

    @Test public void production_jni_transport_fails_closed_without_a_verified_native_session() throws Exception {
        RecordingBridge bridge = new RecordingBridge(3, 29, 0);
        PlannedNativeConnectorPort port = port(bridge, new PlannedNativeConnectorPort.JniPlannedTransport());
        Outcome outcome = new Outcome();

        port.open(input(), outcome);

        assertEquals(1, bridge.opens);
        assertEquals(1, bridge.cancels);
        assertEquals(1, outcome.completions);
    }

    @Test public void injected_runtime_registrations_are_independent_until_their_own_callbacks_finish()
        throws Exception {
        RecordingBridge firstBridge = new RecordingBridge(3, 29, 0);
        RecordingBridge secondBridge = new RecordingBridge(3, 31, 0);
        RecordingTransport firstTransport = new RecordingTransport();
        RecordingTransport secondTransport = new RecordingTransport();
        PlannedNativeConnectorPort first = new PlannedNativeConnectorPort(
            () -> 7,
            RustNativeConnectorJni.PlanFactory.fromHandleResolver(firstBridge,
                ignored -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7)),
            firstTransport
        );
        PlannedNativeConnectorPort second = new PlannedNativeConnectorPort(
            () -> 11,
            RustNativeConnectorJni.PlanFactory.fromHandleResolver(secondBridge,
                ignored -> new RustNativeConnectorJni.NativeHandles(11, 12, 13, 14, 15, 16, 17)),
            secondTransport
        );
        Outcome firstOutcome = new Outcome();
        Outcome secondOutcome = new Outcome();

        first.open(input(), firstOutcome);
        second.open(input(), secondOutcome);
        firstTransport.callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.HOST_KEY_REJECTED
        ));

        assertEquals(7, firstBridge.generation);
        assertEquals(11, secondBridge.generation);
        assertEquals(1, firstBridge.cancels);
        assertEquals(0, secondBridge.cancels);
        assertEquals(1, firstOutcome.completions);
        assertEquals(0, secondOutcome.completions);

        secondTransport.callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE
        ));
        assertEquals(1, secondBridge.cancels);
        assertEquals(1, secondOutcome.completions);
    }

    private static PlannedNativeConnectorPort port(
        RecordingBridge bridge, PlannedNativeConnectorPort.PlannedTransportPort transport
    ) {
        return new PlannedNativeConnectorPort(
            () -> 7,
            RustNativeConnectorJni.PlanFactory.fromHandleResolver(bridge,
                ignored -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7)),
            transport
        );
    }

    private static NativeAuthenticatedSshConnector.NativeConnectionInput input() throws Exception {
        CapturingPort capture = new CapturingPort();
        new NativeAuthenticatedSshConnector(capture).openVerified(request(), result -> { });
        return capture.input;
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request() {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture_user"),
            new ProfileConnectionMetadataSource.KnownHost(ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012")
        );
    }

    private static final class CapturingPort implements NativeAuthenticatedSshConnector.NativeConnectorPort {
        NativeAuthenticatedSshConnector.NativeConnectionInput input;
        @Override public void open(NativeAuthenticatedSshConnector.NativeConnectionInput input,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback) {
            this.input = input;
        }
    }

    private static final class RecordingTransport implements PlannedNativeConnectorPort.PlannedTransportPort {
        int opens;
        RustNativeConnectorJni.Plan plan;
        NativeAuthenticatedSshConnector.NativeOpenCallback callback;
        @Override public void open(RustNativeConnectorJni.Plan plan,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback) {
            opens++;
            this.plan = plan;
            this.callback = callback;
        }
    }

    private static final class RecordingBridge implements RustNativeConnectorJni.NativePlanBridge {
        final int abi;
        final long plan;
        final int cancelResult;
        int generation;
        int cancels;
        int opens;
        RecordingBridge(int abi, long plan, int cancelResult) {
            this.abi = abi; this.plan = plan; this.cancelResult = cancelResult;
        }
        @Override public int abiVersion() { return abi; }
        @Override public long beginAuthenticatedPlan(int generation, RustNativeConnectorJni.NativeHandles handles) {
            this.generation = generation; return plan;
        }
        @Override public int openAuthenticatedPlan(int generation, long plan) { opens++; return 5; }
        @Override public int cancelAuthenticatedPlan(int generation, long plan) {
            cancels++; return cancelResult;
        }
    }

    private static final class Outcome implements NativeAuthenticatedSshConnector.NativeOpenCallback {
        NativeAuthenticatedSshConnector.NativeOpenResult result;
        int completions;
        @Override public void onComplete(NativeAuthenticatedSshConnector.NativeOpenResult result) {
            this.result = result;
            completions++;
        }
    }
}
