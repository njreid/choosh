package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

public final class SshKeyImportCoordinatorTest {
    private static final String FINGERPRINT = "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcde";

    @Test public void importsOnlyOpaqueReferenceAndPublicMetadata() {
        FakePicker picker = new FakePicker();
        FakeReader reader = new FakeReader();
        FakeStore store = new FakeStore();
        FakeBinding binding = new FakeBinding();
        SshKeyImportCoordinator.SshKeyImportOutcome[] result = new SshKeyImportCoordinator.SshKeyImportOutcome[1];
        coordinator(picker, reader, store, binding).requestImport(outcome -> result[0] = outcome);

        picker.select();

        assertEquals(SshKeyImportCoordinator.SshKeyImportCode.IMPORTED, result[0].code());
        assertEquals(SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519, result[0].publicKey().algorithm());
        assertEquals(FINGERPRINT, result[0].publicKey().sha256Fingerprint());
        assertSame(store.credential, binding.bound);
        assertEquals(1, reader.opens.get());
        assertEquals(1, store.imports.get());
        assertEquals(0, store.discards.get());
    }

    @Test public void cancellation_never_opensOrStoresDocument() {
        FakePicker picker = new FakePicker();
        FakeReader reader = new FakeReader();
        FakeStore store = new FakeStore();
        FakeBinding binding = new FakeBinding();
        SshKeyImportCoordinator.SshKeyImportOutcome[] result = new SshKeyImportCoordinator.SshKeyImportOutcome[1];
        coordinator(picker, reader, store, binding).requestImport(outcome -> result[0] = outcome);

        picker.cancel();

        assertEquals(SshKeyImportCoordinator.SshKeyImportCode.CANCELLED, result[0].code());
        assertNull(result[0].publicKey());
        assertEquals(0, reader.opens.get());
        assertEquals(0, store.imports.get());
        assertNull(binding.bound);
    }

    @Test public void invalidDocumentIsTypedAndDoesNotReachKeystore() {
        FakePicker picker = new FakePicker();
        FakeReader reader = new FakeReader();
        reader.fail = true;
        FakeStore store = new FakeStore();
        SshKeyImportCoordinator.SshKeyImportOutcome[] result = new SshKeyImportCoordinator.SshKeyImportOutcome[1];
        coordinator(picker, reader, store, new FakeBinding()).requestImport(outcome -> result[0] = outcome);

        picker.select();

        assertEquals(SshKeyImportCoordinator.SshKeyImportCode.INVALID_DOCUMENT, result[0].code());
        assertEquals(0, store.imports.get());
    }

    @Test public void failedProfileBindingDiscardsNewCredential() {
        FakePicker picker = new FakePicker();
        FakeStore store = new FakeStore();
        FakeBinding binding = new FakeBinding();
        SshKeyImportCoordinator.ImportedSshCredential previous = credential();
        binding.bound = previous;
        binding.fail = true;
        SshKeyImportCoordinator.SshKeyImportOutcome[] result = new SshKeyImportCoordinator.SshKeyImportOutcome[1];
        coordinator(picker, new FakeReader(), store, binding).requestImport(outcome -> result[0] = outcome);

        picker.select();

        assertEquals(SshKeyImportCoordinator.SshKeyImportCode.PROFILE_BINDING_FAILED, result[0].code());
        assertEquals(1, store.discards.get());
        assertSame(store.credential.credentialRef(), store.discarded);
        assertSame(previous, binding.bound);
    }

    @Test public void failedBindingAndCleanupNeverClaimsImportSuccess() {
        FakePicker picker = new FakePicker();
        FakeStore store = new FakeStore();
        store.failDiscard = true;
        FakeBinding binding = new FakeBinding();
        binding.fail = true;
        SshKeyImportCoordinator.SshKeyImportOutcome[] result = new SshKeyImportCoordinator.SshKeyImportOutcome[1];
        coordinator(picker, new FakeReader(), store, binding).requestImport(outcome -> result[0] = outcome);

        picker.select();

        assertEquals(SshKeyImportCoordinator.SshKeyImportCode.CLEANUP_FAILED, result[0].code());
        assertNull(result[0].publicKey());
        assertEquals(1, store.discards.get());
        assertNull(binding.bound);
    }

    @Test public void rejectsPathsPrivateKeyMarkersAndMalformedFingerprints() {
        for (String invalid : new String[] {
            "/data/data/com.termux/files/home/.ssh/id_ed25519",
            "content://provider/key",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "key\nreference",
        }) {
            try {
                new SshKeyImportCoordinator.OpaqueCredentialRef(invalid);
            } catch (IllegalArgumentException expected) {
                continue;
            }
            throw new AssertionError("expected invalid credential reference: " + invalid);
        }
        assertEquals("OpaqueCredentialRef(REDACTED)", new SshKeyImportCoordinator.OpaqueCredentialRef("keystore_42").toString());
        try {
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519, "SHA256:not-a-digest");
        } catch (IllegalArgumentException expected) {
            return;
        }
        throw new AssertionError("expected malformed fingerprint rejection");
    }

    private static SshKeyImportCoordinator coordinator(
        FakePicker picker,
        FakeReader reader,
        FakeStore store,
        FakeBinding binding
    ) {
        return new SshKeyImportCoordinator(picker, reader, store, binding);
    }

    private static final class FakePicker implements SshKeyImportCoordinator.ActivityResultDocumentPicker {
        private SshKeyImportCoordinator.DocumentPickCallback callback;

        @Override public void launchOpenDocument(SshKeyImportCoordinator.DocumentPickCallback callback) {
            this.callback = callback;
        }

        void select() { callback.onResult(SshKeyImportCoordinator.DocumentPickResult.selected(new FakeSelectedDocument())); }
        void cancel() { callback.onResult(SshKeyImportCoordinator.DocumentPickResult.cancelled()); }
    }

    private static final class FakeSelectedDocument implements SshKeyImportCoordinator.SelectedDocument {}
    private static final class FakeDocument implements SshKeyImportCoordinator.SshKeyDocument {}

    private static final class FakeReader implements SshKeyImportCoordinator.SshKeyDocumentReader {
        final AtomicInteger opens = new AtomicInteger();
        boolean fail;

        @Override public SshKeyImportCoordinator.SshKeyDocument open(
            SshKeyImportCoordinator.SelectedDocument document
        ) throws SshKeyImportCoordinator.DocumentReadException {
            opens.incrementAndGet();
            assertTrue(document instanceof FakeSelectedDocument);
            if (fail) throw new SshKeyImportCoordinator.DocumentReadException();
            return new FakeDocument();
        }
    }

    private static final class FakeStore implements SshKeyImportCoordinator.KeystoreCredentialStore {
        final AtomicInteger imports = new AtomicInteger();
        final AtomicInteger discards = new AtomicInteger();
        final SshKeyImportCoordinator.ImportedSshCredential credential = credential();
        SshKeyImportCoordinator.OpaqueCredentialRef discarded;
        boolean failDiscard;

        @Override public SshKeyImportCoordinator.ImportedSshCredential importDocument(
            SshKeyImportCoordinator.SshKeyDocument document
        ) {
            imports.incrementAndGet();
            assertTrue(document instanceof FakeDocument);
            return credential;
        }

        @Override public void discard(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef)
            throws SshKeyImportCoordinator.KeystoreException {
            discards.incrementAndGet();
            discarded = credentialRef;
            if (failDiscard) throw new SshKeyImportCoordinator.KeystoreException();
        }
    }

    private static final class FakeBinding implements SshKeyImportCoordinator.ProfileCredentialBinding {
        SshKeyImportCoordinator.ImportedSshCredential bound;
        boolean fail;

        @Override public void replaceAtomically(SshKeyImportCoordinator.ImportedSshCredential credential)
            throws SshKeyImportCoordinator.ProfileBindingException {
            if (fail) throw new SshKeyImportCoordinator.ProfileBindingException();
            bound = credential;
        }
    }

    private static SshKeyImportCoordinator.ImportedSshCredential credential() {
        return new SshKeyImportCoordinator.ImportedSshCredential(
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519, FINGERPRINT
            )
        );
    }
}
