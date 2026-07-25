package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import org.junit.Test;

/** Headless lifecycle proof for the concrete Android runtime lease adapter. */
public final class BoundedAndroidNativeRuntimeTest {
    @Test public void lease_binds_fixed_identity_socket_and_signer_then_closes_once() throws Exception {
        RecordingSocket opened = new RecordingSocket();
        BoundedAndroidNativeRuntime runtime = new BoundedAndroidNativeRuntime(
            input -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7),
            new BoundedAndroidSocketAdapter((host, port, connect, read) -> opened,
                new BoundedAndroidSocketAdapter.Limits(1, 1, 64)),
            input -> "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti"
                .getBytes(java.nio.charset.StandardCharsets.UTF_8),
            input -> payload -> new byte[] { 9 }
        );
        RustNativeConnectorJni.NativeLease lease = runtime.acquire(capturedInput());
        AndroidRuntimeCallbackPort callbacks = lease.callbacks();

        assertEquals(1, callbacks.metadata(7)[0]);
        assertArrayEquals(new byte[] { 9 }, callbacks.sign(7, new byte[] { 1 }));
        callbacks.write(7, new byte[] { 4, 5 });
        assertArrayEquals(new byte[] { 4, 5 }, opened.output.toByteArray());
        lease.close();
        lease.close();
        assertEquals(1, opened.closes);
        assertThrows(AndroidRuntimeCallbackPort.CallbackException.class,
            () -> callbacks.metadata(7));
    }

    private static NativeAuthenticatedSshConnector.NativeConnectionInput capturedInput() throws Exception {
        final NativeAuthenticatedSshConnector.NativeConnectionInput[] captured = new NativeAuthenticatedSshConnector.NativeConnectionInput[1];
        NativeAuthenticatedSshConnector connector = new NativeAuthenticatedSshConnector((input, callback) -> {
            captured[0] = input;
            callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE));
        });
        connector.openVerified(new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture-user"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ")
        ), result -> { });
        return captured[0];
    }

    private static final class RecordingSocket implements BoundedAndroidSocketAdapter.OpenedSocket {
        final ByteArrayOutputStream output = new ByteArrayOutputStream();
        int closes;
        @Override public InputStream input() { return new ByteArrayInputStream(new byte[0]); }
        @Override public OutputStream output() { return output; }
        @Override public void close() { closes++; }
    }
}
