package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

/** Headless contract test for pointer-free JNI plan admission. */
public final class RustNativeConnectorJniTest {
    @Test public void plan_uses_only_resolved_opaque_handles_and_closes_once() throws Exception {
        CapturingConnector connector = new CapturingConnector();
        new NativeAuthenticatedSshConnector(connector).openVerified(request(), result -> { });
        RecordingBridge bridge = new RecordingBridge();
        RustNativeConnectorJni.PlanFactory factory = new RustNativeConnectorJni.PlanFactory(
            bridge,
            input -> {
                assertEquals("NativeConnectionInput(endpoint=REDACTED, username=REDACTED, knownHost=ED25519, credential=REDACTED, publicKey=ED25519)",
                    input.toString());
                return new RustNativeConnectorJni.NativeHandles(11, 12, 13, 14, 15);
            }
        );

        RustNativeConnectorJni.Plan plan = factory.begin(7, connector.input);
        assertEquals(7, bridge.generation);
        assertEquals("NativeHandles(REDACTED)", bridge.handles.toString());
        plan.close();
        plan.close();
        assertEquals(1, bridge.cancels);
        assertEquals("NativeAuthenticatedPlan(REDACTED)", plan.toString());
    }

    @Test public void incompatible_abi_or_native_rejection_never_yields_a_plan() {
        CapturingConnector connector = new CapturingConnector();
        try {
            new NativeAuthenticatedSshConnector(connector).openVerified(request(), result -> { });
        } catch (AuthenticatedSshOperationCoordinator.SshTransportException exception) {
            throw new AssertionError(exception);
        }
        RustNativeConnectorJni.PlanFactory incompatible = new RustNativeConnectorJni.PlanFactory(
            new RecordingBridge(2, 99), input -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5)
        );
        assertThrows(RustNativeConnectorJni.NativePlanException.class,
            () -> incompatible.begin(1, connector.input));

        RustNativeConnectorJni.PlanFactory rejected = new RustNativeConnectorJni.PlanFactory(
            new RecordingBridge(1, 0), input -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5)
        );
        assertThrows(RustNativeConnectorJni.NativePlanException.class,
            () -> rejected.begin(1, connector.input));
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

    private static final class CapturingConnector implements NativeAuthenticatedSshConnector.NativeConnectorPort {
        NativeAuthenticatedSshConnector.NativeConnectionInput input;

        @Override public void open(
            NativeAuthenticatedSshConnector.NativeConnectionInput input,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback
        ) {
            this.input = input;
            callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE
            ));
        }
    }

    private static final class RecordingBridge implements RustNativeConnectorJni.NativePlanBridge {
        final int abi;
        final long plan;
        int generation;
        RustNativeConnectorJni.NativeHandles handles;
        int cancels;

        RecordingBridge() { this(1, 99); }
        RecordingBridge(int abi, long plan) { this.abi = abi; this.plan = plan; }
        @Override public int abiVersion() { return abi; }
        @Override public long beginAuthenticatedPlan(int generation, RustNativeConnectorJni.NativeHandles handles) {
            this.generation = generation;
            this.handles = handles;
            return plan;
        }
        @Override public int cancelAuthenticatedPlan(int generation, long plan) {
            cancels++;
            return 0;
        }
    }
}
