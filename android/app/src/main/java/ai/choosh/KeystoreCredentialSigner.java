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
    // The native callback result is capped at 65,536 bytes. An Ed25519 SSH signature envelope
    // consumes 87 bytes, so accepting a larger transcript would make a valid provider result
    // impossible to return through the bounded ABI.
    private static final int MAX_SIGNING_PAYLOAD_BYTES = SshWireSignature.maximumEd25519PayloadBytes();
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

    /**
     * Binds one admitted connection to its Android-owned credential before Russh asks for a
     * challenge proof.
     *
     * <p>The returned callback deliberately has no credential or public-key arguments. A native
     * per-challenge callback can therefore supply only the bounded SSH payload; it cannot switch
     * identities between challenges or obtain private-key material.</p>
     */
    public ChallengeSigner beginChallengeSigning(
        HostKeyAdmission admission,
        SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
        SshKeyImportCoordinator.PublicKeyMetadata publicKey
    ) {
        return new ChallengeSigner(
            Objects.requireNonNull(admission, "admission"),
            Objects.requireNonNull(credentialRef, "credentialRef"),
            Objects.requireNonNull(publicKey, "publicKey")
        );
    }

    /**
     * Adapts an admitted Java challenge signer to the native callback contract.
     *
     * <p>The JNI registry retains this callback behind a non-zero opaque handle. Rust receives
     * that handle, never this Java object, credential reference, public-key metadata, or private
     * key material. The eventual JNI registry MUST expose the callback to Russh only after the
     * Rust host-key verifier has admitted the connection.</p>
     */
    public NativeChallengeCallback nativeChallengeCallback(ChallengeSigner signer) {
        ChallengeSigner checked = Objects.requireNonNull(signer, "signer");
        return payload -> {
            try {
                return checked.sign(payload).copyForNativeCallback();
            } catch (SigningException exception) {
                throw new NativeSigningException();
            }
        };
    }

    /** Payload-only callback retained by the Android JNI registry behind an opaque handle. */
    public interface NativeChallengeCallback {
        byte[] sign(byte[] payload) throws NativeSigningException;
    }

    /** Produces one non-secret SSH signature after exact host admission. */
    public Signature sign(SigningRequest request) throws SigningException {
        Objects.requireNonNull(request, "request");
        return beginChallengeSigning(
            request.admission, request.credentialRef, request.publicKey
        ).sign(request.copyPayloadForKeystore());
    }

    /**
     * Per-connection callback for Rust's public-key authentication challenges.
     *
     * <p>It intentionally exposes only {@link #sign(byte[])}. The opaque admission, credential
     * reference, and public-key metadata remain fixed for this connection attempt and never
     * cross the callback boundary as private-key bytes.</p>
     */
    public final class ChallengeSigner {
        private final HostKeyAdmission admission;
        private final SshKeyImportCoordinator.OpaqueCredentialRef credentialRef;
        private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

        private ChallengeSigner(
            HostKeyAdmission admission,
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey
        ) {
            this.admission = admission;
            this.credentialRef = credentialRef;
            this.publicKey = publicKey;
        }

        /** Signs one bounded SSH authentication payload with the connection's fixed identity. */
        public Signature sign(byte[] payload) throws SigningException {
            Objects.requireNonNull(payload, "payload");
            if (payload.length == 0 || payload.length > MAX_SIGNING_PAYLOAD_BYTES) {
                throw new IllegalArgumentException("invalid signing payload length");
            }
            byte[] payloadCopy = payload.clone();
            try {
                return new Signature(backend.sign(credentialRef, publicKey, payloadCopy));
            } catch (KeystoreFailure | IllegalArgumentException | NullPointerException exception) {
                // The native callback receives one stable, content-free failure for both backend
                // and malformed-output errors. In particular no provider detail reaches Rust.
                throw new SigningException();
            }
        }

        @Override public String toString() {
            return "ChallengeSigner(admission=REDACTED, credential=REDACTED, publicKey="
                + publicKey.algorithm() + ")";
        }
    }

    /**
     * Android outer adapter. Implementations resolve the opaque reference without exporting a key.
     *
     * <p>For {@code ED25519}, Android Keystore's raw 64-byte {@code Signature} result MUST be
     * converted with {@link SshWireSignature#appendEd25519(byte[], byte[])} before returning. The
     * result is the Russh custom-signer value: the original payload followed by one SSH
     * wire-encoded signature. It is not an Android provider signature alone.</p>
     */
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

    /** Content-free result exposed by the JNI callback boundary. */
    public static final class NativeSigningException extends Exception {
        public NativeSigningException() { super(); }
    }
}
