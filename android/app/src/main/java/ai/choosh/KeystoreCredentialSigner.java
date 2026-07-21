package ai.choosh;

import java.util.Objects;

/**
 * Headless boundary for producing an SSH signature without exposing credential material.
 *
 * <p>The Android composition root supplies a {@link SigningBackend} backed by Android Keystore
 * and app-private encrypted credential storage. The only credential input is its opaque store
 * reference. This class has no private-key, passphrase, document, URI, path, or alias accessor.
 * A signing request is unrepresentable until {@link #admitExactHost} has compared the presented
 * host identity with the persisted exact identity. The eventual Rust/JNI transport MUST retain
 * the same gate; this Java seam is deliberately not a substitute for transport-side admission.</p>
 */
public final class KeystoreCredentialSigner {
    private static final int MAX_SIGNING_PAYLOAD_BYTES = 65_536;
    private static final int MAX_SIGNATURE_BYTES = 16_384;

    private final SigningBackend backend;

    public KeystoreCredentialSigner(SigningBackend backend) {
        this.backend = Objects.requireNonNull(backend, "backend");
    }

    /**
     * Compares both the persisted host-key algorithm and canonical fingerprint before signing.
     * A mismatch yields no admission token and therefore cannot reach the Keystore backend.
     */
    public static HostKeyAdmission admitExactHost(
        ProfileConnectionMetadataSource.KnownHost expected,
        PresentedHostKey presented
    ) throws HostKeyRejectedException {
        Objects.requireNonNull(expected, "expected");
        Objects.requireNonNull(presented, "presented");
        if (expected.algorithm() != presented.algorithm
            || !expected.sha256FingerprintForVerifier().equals(presented.sha256Fingerprint)) {
            throw new HostKeyRejectedException();
        }
        return new HostKeyAdmission();
    }

    /** Produces one non-secret SSH signature after exact host admission. */
    public Signature sign(SigningRequest request) throws SigningException {
        Objects.requireNonNull(request, "request");
        try {
            return new Signature(backend.sign(
                request.credentialRef,
                request.publicKey,
                request.copyPayloadForKeystore()
            ));
        } catch (KeystoreFailure exception) {
            throw new SigningException();
        }
    }

    /** Android outer adapter. Implementations resolve the opaque reference without exporting a key. */
    public interface SigningBackend {
        byte[] sign(
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey,
            byte[] payload
        ) throws KeystoreFailure;
    }

    /** Non-secret network identity observed during the SSH host-key callback. */
    public static final class PresentedHostKey {
        private final ProfileConnectionMetadataSource.HostKeyAlgorithm algorithm;
        private final String sha256Fingerprint;

        public PresentedHostKey(
            ProfileConnectionMetadataSource.HostKeyAlgorithm algorithm,
            String sha256Fingerprint
        ) {
            this.algorithm = Objects.requireNonNull(algorithm, "algorithm");
            this.sha256Fingerprint = new ProfileConnectionMetadataSource.KnownHost(
                algorithm, sha256Fingerprint
            ).sha256FingerprintForVerifier();
        }

        @Override public String toString() {
            return "PresentedHostKey(algorithm=" + algorithm + ", fingerprint=REDACTED)";
        }
    }

    /** Opaque proof that an exact host key was admitted for this connection attempt. */
    public static final class HostKeyAdmission {
        private HostKeyAdmission() { }

        @Override public String toString() { return "HostKeyAdmission(REDACTED)"; }
    }

    /** Bounded signing input whose construction requires an opaque exact-host admission. */
    public static final class SigningRequest {
        private final HostKeyAdmission admission;
        private final SshKeyImportCoordinator.OpaqueCredentialRef credentialRef;
        private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;
        private final byte[] payload;

        public SigningRequest(
            HostKeyAdmission admission,
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey,
            byte[] payload
        ) {
            this.admission = Objects.requireNonNull(admission, "admission");
            this.credentialRef = Objects.requireNonNull(credentialRef, "credentialRef");
            this.publicKey = Objects.requireNonNull(publicKey, "publicKey");
            Objects.requireNonNull(payload, "payload");
            if (payload.length == 0 || payload.length > MAX_SIGNING_PAYLOAD_BYTES) {
                throw new IllegalArgumentException("invalid signing payload length");
            }
            this.payload = payload.clone();
        }

        private byte[] copyPayloadForKeystore() { return payload.clone(); }

        @Override public String toString() {
            return "SigningRequest(admission=REDACTED, credential=REDACTED, publicKey="
                + publicKey.algorithm() + ", payloadBytes=" + payload.length + ")";
        }
    }

    /** Bounded non-secret signature returned only to the native SSH callback. */
    public static final class Signature {
        private final byte[] bytes;

        public Signature(byte[] bytes) {
            Objects.requireNonNull(bytes, "bytes");
            if (bytes.length == 0 || bytes.length > MAX_SIGNATURE_BYTES) {
                throw new IllegalArgumentException("invalid SSH signature length");
            }
            this.bytes = bytes.clone();
        }

        /** Intended only for the native SSH callback adapter. */
        public byte[] copyForNativeCallback() { return bytes.clone(); }

        @Override public String toString() { return "SshSignature(REDACTED)"; }
    }

    public static final class HostKeyRejectedException extends Exception {
        public HostKeyRejectedException() { super(); }
    }

    public static final class KeystoreFailure extends Exception {
        public KeystoreFailure() { super(); }
    }

    public static final class SigningException extends Exception {
        public SigningException() { super(); }
    }
}
