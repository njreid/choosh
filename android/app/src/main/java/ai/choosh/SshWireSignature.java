package ai.choosh;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Objects;

/**
 * Encodes one Android Keystore Ed25519 proof for Russh's custom-signer boundary.
 *
 * <p>Russh passes the SSH authentication transcript to its custom signer and expects the
 * returned value to retain that transcript followed by one SSH {@code string} containing the
 * algorithm name and raw signature. Android's {@code Signature} API returns only the 64-byte
 * Ed25519 proof, so this small outer adapter performs the protocol framing without exposing a
 * key, alias, endpoint, or command.</p>
 */
public final class SshWireSignature {
    private static final byte[] ED25519 = "ssh-ed25519".getBytes(StandardCharsets.US_ASCII);
    private static final int ED25519_SIGNATURE_BYTES = 64;
    private static final int SIGNATURE_CONTENT_BYTES = 4 + ED25519.length + 4 + ED25519_SIGNATURE_BYTES;
    private static final int ENCODED_SIGNATURE_BYTES = Integer.BYTES + SIGNATURE_CONTENT_BYTES;

    private SshWireSignature() { }

    /** Maximum transcript size that still fits a 65,536-byte JNI callback result. */
    public static int maximumEd25519PayloadBytes() {
        return 65_536 - ENCODED_SIGNATURE_BYTES;
    }

    /** Returns {@code payload || string(string("ssh-ed25519") || string(rawSignature))}. */
    public static byte[] appendEd25519(byte[] payload, byte[] rawSignature) {
        Objects.requireNonNull(payload, "payload");
        Objects.requireNonNull(rawSignature, "rawSignature");
        if (payload.length == 0 || payload.length > maximumEd25519PayloadBytes()
            || rawSignature.length != ED25519_SIGNATURE_BYTES) {
            throw new IllegalArgumentException("invalid SSH Ed25519 signature input");
        }
        ByteBuffer result = ByteBuffer.allocate(payload.length + ENCODED_SIGNATURE_BYTES);
        result.put(payload);
        result.putInt(SIGNATURE_CONTENT_BYTES);
        result.putInt(ED25519.length);
        result.put(ED25519);
        result.putInt(rawSignature.length);
        result.put(rawSignature);
        return result.array();
    }

    /** Returns the Russh signer value for a raw RSA-SHA256 signature. */
    public static byte[] appendRsaSha256(byte[] payload, byte[] rawSignature) {
        Objects.requireNonNull(payload, "payload");
        Objects.requireNonNull(rawSignature, "rawSignature");
        byte[] algorithm = "rsa-sha2-256".getBytes(StandardCharsets.US_ASCII);
        if (payload.length == 0 || rawSignature.length == 0) {
            throw new IllegalArgumentException("invalid SSH RSA signature input");
        }
        int content = 4 + algorithm.length + 4 + rawSignature.length;
        ByteBuffer result = ByteBuffer.allocate(payload.length + 4 + content);
        result.put(payload).putInt(content).putInt(algorithm.length).put(algorithm)
            .putInt(rawSignature.length).put(rawSignature);
        return result.array();
    }
}
