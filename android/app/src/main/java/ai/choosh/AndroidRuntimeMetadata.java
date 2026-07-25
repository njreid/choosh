package ai.choosh;

import java.nio.charset.StandardCharsets;
import java.util.Objects;

/** Versioned, bounded non-secret identity metadata for one Android runtime lease. */
public final class AndroidRuntimeMetadata {
    private static final int VERSION = 1;
    private static final int MAX_CAPSULE_BYTES = 256;

    private AndroidRuntimeMetadata() { }

    /** Encodes canonical username, exact host fingerprint, and public-key identity only. */
    public static byte[] encode(
        ProfileConnectionMetadataSource.SshUsername username,
        ProfileConnectionMetadataSource.KnownHost knownHost,
        SshKeyImportCoordinator.PublicKeyMetadata publicKey
    ) {
        byte[] user = utf8(Objects.requireNonNull(username, "username").valueForNativeConnector());
        byte[] host = utf8(Objects.requireNonNull(knownHost, "knownHost").sha256FingerprintForVerifier());
        byte[] algorithm = utf8(Objects.requireNonNull(publicKey, "publicKey").algorithm().name());
        byte[] fingerprint = utf8(publicKey.sha256Fingerprint());
        int length = 1 + 4 + user.length + host.length + algorithm.length + fingerprint.length;
        if (length > MAX_CAPSULE_BYTES) throw new IllegalArgumentException("runtime metadata too large");
        byte[] result = new byte[length];
        int offset = 0;
        result[offset++] = VERSION;
        offset = copy(user, result, offset);
        offset = copy(host, result, offset);
        offset = copy(algorithm, result, offset);
        copy(fingerprint, result, offset);
        return result;
    }

    private static byte[] utf8(String value) {
        byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        if (bytes.length == 0 || bytes.length > 255) throw new IllegalArgumentException("invalid metadata field");
        return bytes;
    }

    private static int copy(byte[] field, byte[] target, int offset) {
        target[offset++] = (byte) field.length;
        System.arraycopy(field, 0, target, offset, field.length);
        return offset + field.length;
    }
}
