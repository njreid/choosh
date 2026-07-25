package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import org.junit.Test;

/** Headless proof that the outer composition remains lazy and attempt-scoped. */
public final class AndroidRuntimeCompositionTest {
    @Test public void creates_fresh_lazy_runtime_with_injected_bounded_capabilities() throws Exception {
        RecordingOpener opener = new RecordingOpener();
        int[] keyCalls = {0};
        int[] signerCalls = {0};
        AndroidRuntimeComposition composition = new AndroidRuntimeComposition(
            input -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7),
            opener,
            new BoundedAndroidSocketAdapter.Limits(123, 456, 64),
            input -> {
                keyCalls[0]++;
                assertEquals("android_keystore_key_42", input.credentialRef().valueForCredentialStore());
                return "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti"
                    .getBytes(java.nio.charset.StandardCharsets.UTF_8);
            },
            input -> {
                signerCalls[0]++;
                assertEquals("fixture-user", input.username().valueForNativeConnector());
                return payload -> new byte[] {8, 9};
            }
        );

        BoundedAndroidNativeRuntime first = composition.newRuntime();
        BoundedAndroidNativeRuntime second = composition.newRuntime();
        assertEquals(0, opener.opens);

        RustNativeConnectorJni.NativeLease lease = first.acquire(input());
        assertEquals(1, opener.opens);
        assertEquals("ssh-fixture.example", opener.host);
        assertEquals(22, opener.port);
        assertEquals(123, opener.connectTimeout);
        assertEquals(456, opener.readTimeout);
        assertEquals(1, keyCalls[0]);
        assertEquals(1, signerCalls[0]);
        assertArrayEquals(new byte[] {8, 9}, lease.callbacks().sign(7, new byte[] {1}));
        lease.close();
        assertEquals(1, opener.socket.closes);

        // A factory invocation has no shared lease or socket state.
        RustNativeConnectorJni.NativeLease laterLease = second.acquire(input());
        laterLease.close();
        assertEquals(2, opener.opens);
        assertEquals(2, opener.socket.closes);
    }

    private static NativeAuthenticatedSshConnector.NativeConnectionInput input() throws Exception {
        final NativeAuthenticatedSshConnector.NativeConnectionInput[] captured = new NativeAuthenticatedSshConnector.NativeConnectionInput[1];
        new NativeAuthenticatedSshConnector((value, callback) -> {
            captured[0] = value;
            callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE));
        }).openVerified(new AuthenticatedSshOperationCoordinator.ConnectionRequest(
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
        ), ignored -> { });
        return captured[0];
    }

    private static final class RecordingOpener implements BoundedAndroidSocketAdapter.SocketOpener {
        int opens;
        String host;
        int port;
        int connectTimeout;
        int readTimeout;
        final RecordingSocket socket = new RecordingSocket();

        @Override public BoundedAndroidSocketAdapter.OpenedSocket open(
            String value, int port, int connect, int read
        ) {
            opens++;
            host = value;
            this.port = port;
            connectTimeout = connect;
            readTimeout = read;
            return socket;
        }
    }

    private static final class RecordingSocket implements BoundedAndroidSocketAdapter.OpenedSocket {
        int closes;
        @Override public InputStream input() { return new ByteArrayInputStream(new byte[0]); }
        @Override public OutputStream output() { return new ByteArrayOutputStream(); }
        @Override public void close() { closes++; }
    }
}
