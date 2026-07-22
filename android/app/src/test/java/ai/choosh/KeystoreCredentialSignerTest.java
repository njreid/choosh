package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

/** Headless proof that exact host admission gates opaque Keystore signing. */
public final class KeystoreCredentialSignerTest {
    @Test public void exact_host_admission_precedes_opaque_keystore_signing() throws Exception {
        RecordingBackend backend = new RecordingBackend();
        KeystoreCredentialSigner signer = new KeystoreCredentialSigner(backend);
        byte[] payload = new byte[] {1, 2, 3};
        KeystoreCredentialSigner.HostKeyAdmission admission = KeystoreCredentialSigner.admitExactHost(
            knownHost(), presentedHost()
        );

        KeystoreCredentialSigner.Signature signature = signer.sign(
            new KeystoreCredentialSigner.SigningRequest(admission, credential(), publicKey(), payload)
        );

        assertEquals(1, backend.calls.get());
        assertArrayEquals(new byte[] {1, 2, 3}, backend.payload);
        assertArrayEquals(new byte[] {9, 8}, signature.copyForNativeCallback());
        payload[0] = 7;
        assertArrayEquals(new byte[] {1, 2, 3}, backend.payload);
    }

    @Test public void admitted_challenge_callback_binds_identity_and_accepts_only_payloads()
        throws Exception {
        RecordingBackend backend = new RecordingBackend();
        KeystoreCredentialSigner signer = new KeystoreCredentialSigner(backend);
        KeystoreCredentialSigner.ChallengeSigner callback = signer.beginChallengeSigning(
            KeystoreCredentialSigner.admitExactHost(knownHost(), presentedHost()),
            credential(), publicKey()
        );

        byte[] firstPayload = new byte[] {4, 5};
        assertArrayEquals(new byte[] {9, 8}, callback.sign(firstPayload).copyForNativeCallback());
        firstPayload[0] = 0;
        assertArrayEquals(new byte[] {4, 5}, backend.payload);
        assertEquals(1, backend.calls.get());
        assertEquals("ChallengeSigner(admission=REDACTED, credential=REDACTED, publicKey=ED25519)",
            callback.toString());

        assertThrows(IllegalArgumentException.class, () -> callback.sign(new byte[0]));
        assertEquals(1, backend.calls.get());
    }

    @Test public void malformed_keystore_output_becomes_a_typed_callback_failure() throws Exception {
        KeystoreCredentialSigner signer = new KeystoreCredentialSigner((ref, key, payload) -> null);
        KeystoreCredentialSigner.ChallengeSigner callback = signer.beginChallengeSigning(
            KeystoreCredentialSigner.admitExactHost(knownHost(), presentedHost()),
            credential(), publicKey()
        );

        assertThrows(KeystoreCredentialSigner.SigningException.class,
            () -> callback.sign(new byte[] {1}));
    }

    @Test public void native_callback_accepts_only_payloads_and_maps_signer_failures() throws Exception {
        RecordingBackend backend = new RecordingBackend();
        KeystoreCredentialSigner signer = new KeystoreCredentialSigner(backend);
        KeystoreCredentialSigner.NativeChallengeCallback callback = signer.nativeChallengeCallback(
            signer.beginChallengeSigning(
                KeystoreCredentialSigner.admitExactHost(knownHost(), presentedHost()),
                credential(), publicKey()
            )
        );

        byte[] payload = new byte[] {7, 8};
        assertArrayEquals(new byte[] {9, 8}, callback.sign(payload));
        payload[0] = 0;
        assertArrayEquals(new byte[] {7, 8}, backend.payload);

        KeystoreCredentialSigner failing = new KeystoreCredentialSigner((ref, key, bytes) -> null);
        KeystoreCredentialSigner.NativeChallengeCallback failingCallback = failing.nativeChallengeCallback(
            failing.beginChallengeSigning(
                KeystoreCredentialSigner.admitExactHost(knownHost(), presentedHost()),
                credential(), publicKey()
            )
        );
        assertThrows(KeystoreCredentialSigner.NativeSigningException.class,
            () -> failingCallback.sign(new byte[] {1}));
    }

    @Test public void changed_or_different_algorithm_host_never_reaches_signer() {
        RecordingBackend backend = new RecordingBackend();

        assertThrows(KeystoreCredentialSigner.HostKeyRejectedException.class, () ->
            KeystoreCredentialSigner.admitExactHost(knownHost(), new KeystoreCredentialSigner.PresentedHostKey(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789013"
            ))
        );
        assertThrows(KeystoreCredentialSigner.HostKeyRejectedException.class, () ->
            KeystoreCredentialSigner.admitExactHost(knownHost(), new KeystoreCredentialSigner.PresentedHostKey(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.RSA,
                "SHA256:0123456789012345678901234567890123456789012"
            ))
        );
        assertEquals(0, backend.calls.get());
    }

    @Test public void keystore_failure_is_typed_and_secret_values_stay_redacted() throws Exception {
        KeystoreCredentialSigner signer = new KeystoreCredentialSigner((ref, key, payload) -> {
            throw new KeystoreCredentialSigner.KeystoreFailure();
        });
        KeystoreCredentialSigner.HostKeyAdmission admission = KeystoreCredentialSigner.admitExactHost(
            knownHost(), presentedHost()
        );
        KeystoreCredentialSigner.SigningRequest request = new KeystoreCredentialSigner.SigningRequest(
            admission, credential(), publicKey(), new byte[] {1}
        );

        assertThrows(KeystoreCredentialSigner.SigningException.class, () -> signer.sign(request));
        assertEquals("SigningRequest(admission=REDACTED, credential=REDACTED, publicKey=ED25519, payloadBytes=1)",
            request.toString());
        assertEquals("OpaqueCredentialRef(REDACTED)", credential().toString());
    }

    private static ProfileConnectionMetadataSource.KnownHost knownHost() {
        return new ProfileConnectionMetadataSource.KnownHost(
            ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
            "SHA256:0123456789012345678901234567890123456789012"
        );
    }

    private static KeystoreCredentialSigner.PresentedHostKey presentedHost() {
        return new KeystoreCredentialSigner.PresentedHostKey(
            ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
            "SHA256:0123456789012345678901234567890123456789012"
        );
    }

    private static SshKeyImportCoordinator.OpaqueCredentialRef credential() {
        return new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42");
    }

    private static SshKeyImportCoordinator.PublicKeyMetadata publicKey() {
        return new SshKeyImportCoordinator.PublicKeyMetadata(
            SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
            "SHA256:0123456789012345678901234567890123456789012"
        );
    }

    private static final class RecordingBackend implements KeystoreCredentialSigner.SigningBackend {
        final AtomicInteger calls = new AtomicInteger();
        byte[] payload;

        @Override public byte[] sign(
            SshKeyImportCoordinator.OpaqueCredentialRef credentialRef,
            SshKeyImportCoordinator.PublicKeyMetadata publicKey,
            byte[] payload
        ) {
            calls.incrementAndGet();
            this.payload = payload.clone();
            return new byte[] {9, 8};
        }
    }
}
