package ai.choosh;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.util.Objects;

/**
 * Android-owned, constructor-injected TCP stream adapter for the native SSH registry.
 *
 * <p>This class is deliberately outside the policy and JNI layers: it accepts an already
 * validated {@link ProfileConnectionMetadataSource.SshEndpoint}, owns Java socket objects, and
 * exposes only bounded byte reads and writes. A future Android registry may associate the
 * returned {@link Connection} with an opaque native handle; neither the socket nor endpoint
 * string crosses JNI directly.</p>
 */
public final class BoundedAndroidSocketAdapter {
    private final SocketOpener opener;
    private final Limits limits;

    public BoundedAndroidSocketAdapter(SocketOpener opener, Limits limits) {
        this.opener = Objects.requireNonNull(opener, "opener");
        this.limits = Objects.requireNonNull(limits, "limits");
    }

    /** Opens one Android-owned socket using only the separately validated endpoint fields. */
    public Connection open(ProfileConnectionMetadataSource.SshEndpoint endpoint)
        throws SocketOpenException {
        Objects.requireNonNull(endpoint, "endpoint");
        final OpenedSocket socket;
        try {
            socket = opener.open(
                endpoint.hostForNativeConnector(), endpoint.portForNativeConnector(),
                limits.connectTimeoutMillis(), limits.readTimeoutMillis()
            );
            return new Connection(socket, limits.maxOperationBytes());
        } catch (IOException | RuntimeException exception) {
            // Callers receive a stable, content-free transport failure; endpoint/provider detail
            // is intentionally not propagated to UI, JNI, or protocol code.
            throw new SocketOpenException();
        }
    }

    /** Constructor-validated per-socket limits. */
    public static final class Limits {
        private static final int MAX_TIMEOUT_MILLIS = 120_000;
        private static final int MAX_OPERATION_BYTES = 65_536;
        private final int connectTimeoutMillis;
        private final int readTimeoutMillis;
        private final int maxOperationBytes;

        public Limits(int connectTimeoutMillis, int readTimeoutMillis, int maxOperationBytes) {
            if (connectTimeoutMillis <= 0 || connectTimeoutMillis > MAX_TIMEOUT_MILLIS
                || readTimeoutMillis <= 0 || readTimeoutMillis > MAX_TIMEOUT_MILLIS
                || maxOperationBytes <= 0 || maxOperationBytes > MAX_OPERATION_BYTES) {
                throw new IllegalArgumentException("invalid socket limits");
            }
            this.connectTimeoutMillis = connectTimeoutMillis;
            this.readTimeoutMillis = readTimeoutMillis;
            this.maxOperationBytes = maxOperationBytes;
        }

        int connectTimeoutMillis() { return connectTimeoutMillis; }
        int readTimeoutMillis() { return readTimeoutMillis; }
        int maxOperationBytes() { return maxOperationBytes; }
    }

    /** Injected production/JVM socket-opening boundary. */
    public interface SocketOpener {
        OpenedSocket open(String host, int port, int connectTimeoutMillis, int readTimeoutMillis)
            throws IOException;
    }

    /** Java socket boundary retained entirely on the Android side of the native registry. */
    public interface OpenedSocket extends AutoCloseable {
        InputStream input() throws IOException;
        OutputStream output() throws IOException;
        @Override void close() throws IOException;
    }

    /** JVM implementation; construction of the socket itself remains injectable for tests. */
    public static final class JvmSocketOpener implements SocketOpener {
        private final SocketFactory sockets;

        public JvmSocketOpener(SocketFactory sockets) {
            this.sockets = Objects.requireNonNull(sockets, "sockets");
        }

        @Override public OpenedSocket open(
            String host, int port, int connectTimeoutMillis, int readTimeoutMillis
        ) throws IOException {
            Socket socket = sockets.create();
            try {
                socket.connect(new InetSocketAddress(host, port), connectTimeoutMillis);
                socket.setSoTimeout(readTimeoutMillis);
                return new JvmOpenedSocket(socket);
            } catch (IOException | RuntimeException exception) {
                try {
                    socket.close();
                } catch (IOException ignored) {
                    // The content-free opening failure remains authoritative.
                }
                throw exception;
            }
        }
    }

    /** Constructor injection keeps actual JVM networking out of headless tests. */
    public interface SocketFactory {
        Socket create() throws IOException;
    }

    private static final class JvmOpenedSocket implements OpenedSocket {
        private final Socket socket;
        JvmOpenedSocket(Socket socket) { this.socket = socket; }
        @Override public InputStream input() throws IOException { return socket.getInputStream(); }
        @Override public OutputStream output() throws IOException { return socket.getOutputStream(); }
        @Override public void close() throws IOException { socket.close(); }
    }

    /** One-close-only bounded stream held by the Android-owned registry. */
    public static final class Connection implements AutoCloseable {
        private final OpenedSocket socket;
        private final InputStream input;
        private final OutputStream output;
        private final int maxOperationBytes;
        private boolean closed;

        private Connection(OpenedSocket socket, int maxOperationBytes) throws IOException {
            this.socket = Objects.requireNonNull(socket, "socket");
            this.maxOperationBytes = maxOperationBytes;
            try {
                input = socket.input();
                output = socket.output();
            } catch (IOException | RuntimeException exception) {
                try {
                    socket.close();
                } catch (IOException ignored) {
                    // Preserve the original typed opening failure.
                }
                throw exception;
            }
        }

        public int read(byte[] destination, int offset, int length) throws SocketIoException {
            validateRange(destination, offset, length);
            ensureOpen();
            try {
                return input.read(destination, offset, length);
            } catch (IOException | RuntimeException exception) {
                throw new SocketIoException();
            }
        }

        public void write(byte[] source, int offset, int length) throws SocketIoException {
            validateRange(source, offset, length);
            ensureOpen();
            try {
                output.write(source, offset, length);
                output.flush();
            } catch (IOException | RuntimeException exception) {
                throw new SocketIoException();
            }
        }

        @Override public void close() throws SocketIoException {
            if (closed) return;
            closed = true;
            try {
                socket.close();
            } catch (IOException | RuntimeException exception) {
                throw new SocketIoException();
            }
        }

        private void ensureOpen() throws SocketIoException {
            if (closed) throw new SocketIoException();
        }

        private void validateRange(byte[] bytes, int offset, int length) {
            Objects.requireNonNull(bytes, "bytes");
            if (offset < 0 || length <= 0 || length > maxOperationBytes || offset > bytes.length - length) {
                throw new IllegalArgumentException("invalid bounded socket range");
            }
        }
    }

    /** Stable opening failure without endpoint, DNS, or platform detail. */
    public static final class SocketOpenException extends Exception {
        public SocketOpenException() { super(); }
    }

    /** Stable I/O failure without endpoint, stream, or platform detail. */
    public static final class SocketIoException extends Exception {
        public SocketIoException() { super(); }
    }
}
