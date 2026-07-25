package ai.choosh;

/**
 * Narrow Java object retained by exactly one native runtime-plan allocation.
 *
 * <p>The JNI bridge invokes these methods with an opaque lease only. Implementations own the
 * socket and Keystore callback registrations and must reject stale or released leases. They
 * cannot receive a host, path, command, credential selector, or public-key selector.</p>
 */
public interface AndroidRuntimeCallbackPort {
    /**
     * Returns the versioned, non-secret connection metadata fixed when this lease was created.
     *
     * <p>The capsule contains only canonical username, exact persisted host-key identity, and
     * public-key identity metadata. It contains no endpoint, path, command, credential reference,
     * private-key material, or signature.</p>
     */
    byte[] metadata(long runtimeLease) throws CallbackException;

    /**
     * Returns the canonical OpenSSH public key fixed to this signing lease.
     *
     * <p>This is public identity material only. Rust parses it and compares its SHA-256
     * fingerprint with {@link #metadata(long)} before it can create the signer; mismatches are
     * rejected without a signing callback.</p>
     */
    byte[] publicKey(long runtimeLease) throws CallbackException;

    /** Returns at most {@code maximumBytes} socket bytes; an empty array is EOF. */
    byte[] read(long runtimeLease, int maximumBytes) throws CallbackException;

    /** Writes one bounded, copied socket buffer. */
    void write(long runtimeLease, byte[] bytes) throws CallbackException;

    /** Signs one bounded SSH challenge under the lease's fixed admitted identity. */
    byte[] sign(long runtimeLease, byte[] payload) throws CallbackException;

    /** Invalidates the lease and releases its Android-owned resources exactly once. */
    void close(long runtimeLease) throws CallbackException;

    /** Stable content-free callback failure exposed across JNI. */
    final class CallbackException extends Exception {
        public CallbackException() { super(); }
    }
}
