package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.net.SocketAddress;
import org.junit.Test;

/** Headless coverage for Android-owned bounded socket lifecycle and failure projection. */
public final class BoundedAndroidSocketAdapterTest {
    @Test public void endpoint_opens_once_and_stream_operations_are_bounded() throws Exception {
        RecordingSocket socket = new RecordingSocket(new byte[] {1, 2, 3});
        BoundedAndroidSocketAdapter adapter = new BoundedAndroidSocketAdapter(
            (host, port, connectTimeout, readTimeout) -> {
                assertEquals("ssh-fixture.example", host);
                assertEquals(22, port);
                assertEquals(500, connectTimeout);
                assertEquals(700, readTimeout);
                return socket;
            },
            new BoundedAndroidSocketAdapter.Limits(500, 700, 3)
        );

        BoundedAndroidSocketAdapter.Connection connection = adapter.open(endpoint());
        byte[] read = new byte[3];
        assertEquals(3, connection.read(read, 0, 3));
        assertArrayEquals(new byte[] {1, 2, 3}, read);
        connection.write(new byte[] {9, 8, 7}, 0, 3);
        assertArrayEquals(new byte[] {9, 8, 7}, socket.output.toByteArray());
        assertThrows(IllegalArgumentException.class, () -> connection.read(new byte[4], 0, 4));
        connection.close();
        connection.close();
        assertEquals(1, socket.closes);
        assertThrows(BoundedAndroidSocketAdapter.SocketIoException.class,
            () -> connection.write(new byte[] {1}, 0, 1));
    }

    @Test public void opening_and_io_failures_are_content_free_and_close_partial_socket() {
        BoundedAndroidSocketAdapter failingOpen = new BoundedAndroidSocketAdapter(
            (host, port, connectTimeout, readTimeout) -> { throw new IOException("secret endpoint"); },
            new BoundedAndroidSocketAdapter.Limits(1, 1, 1)
        );
        assertThrows(BoundedAndroidSocketAdapter.SocketOpenException.class,
            () -> failingOpen.open(endpoint()));

        RecordingSocket socket = new RecordingSocket(new byte[] {1});
        socket.failWrites = true;
        BoundedAndroidSocketAdapter adapter = new BoundedAndroidSocketAdapter(
            (host, port, connectTimeout, readTimeout) -> socket,
            new BoundedAndroidSocketAdapter.Limits(1, 1, 1)
        );
        try {
            adapter.open(endpoint()).write(new byte[] {1}, 0, 1);
        } catch (BoundedAndroidSocketAdapter.SocketIoException expected) {
            assertEquals(null, expected.getMessage());
            return;
        } catch (Exception unexpected) {
            throw new AssertionError(unexpected);
        }
        throw new AssertionError("expected typed I/O failure");
    }

    @Test public void injected_jvm_opener_applies_timeouts_without_live_networking() throws Exception {
        RecordingJvmSocket socket = new RecordingJvmSocket();
        BoundedAndroidSocketAdapter.JvmSocketOpener opener = new BoundedAndroidSocketAdapter.JvmSocketOpener(
            () -> socket
        );

        BoundedAndroidSocketAdapter.OpenedSocket opened = opener.open("ssh-fixture.example", 22, 500, 700);
        assertEquals(500, socket.connectTimeout);
        assertEquals(700, socket.readTimeout);
        opened.close();
        assertEquals(1, socket.closes);
    }

    private static ProfileConnectionMetadataSource.SshEndpoint endpoint() {
        return new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22);
    }

    private static final class RecordingSocket implements BoundedAndroidSocketAdapter.OpenedSocket {
        final ByteArrayInputStream input;
        final ByteArrayOutputStream output = new ByteArrayOutputStream();
        boolean failWrites;
        int closes;

        RecordingSocket(byte[] input) { this.input = new ByteArrayInputStream(input); }
        @Override public InputStream input() { return input; }
        @Override public OutputStream output() {
            return new OutputStream() {
                @Override public void write(int value) throws IOException {
                    if (failWrites) throw new IOException("secret write failure");
                    output.write(value);
                }
            };
        }
        @Override public void close() { closes++; }
    }

    private static final class RecordingJvmSocket extends Socket {
        int connectTimeout;
        int readTimeout;
        int closes;
        @Override public void connect(SocketAddress endpoint, int timeout) { connectTimeout = timeout; }
        @Override public void setSoTimeout(int timeout) { readTimeout = timeout; }
        @Override public InputStream getInputStream() { return new ByteArrayInputStream(new byte[0]); }
        @Override public OutputStream getOutputStream() { return new ByteArrayOutputStream(); }
        @Override public synchronized void close() { closes++; }
    }
}
