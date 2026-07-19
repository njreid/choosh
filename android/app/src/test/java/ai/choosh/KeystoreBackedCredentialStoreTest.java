package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertSame;

import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

/** Deterministic proof for transactional, opaque credential-store composition. */
public final class KeystoreBackedCredentialStoreTest {
    @Test public void persistsPreparedCredentialAtomicallyAndReturnsOnlyMetadata() throws Exception {
        FakeImporter importer = new FakeImporter();
        FakeEncryptedStore store = new FakeEncryptedStore();
        KeystoreBackedCredentialStore credentialStore = new KeystoreBackedCredentialStore(importer, store);

        SshKeyImportCoordinator.ImportedSshCredential imported = credentialStore.importDocument(new FakeDocument());

        assertSame(importer.prepared.credential(), imported);
        assertSame(importer.prepared, store.replaced);
        assertEquals(0, importer.aborts.get());
    }

    @Test public void failedEncryptedWriteAbortsPreparedCredential() throws Exception {
        FakeImporter importer = new FakeImporter();
        FakeEncryptedStore store = new FakeEncryptedStore();
        store.failReplace = true;
        KeystoreBackedCredentialStore credentialStore = new KeystoreBackedCredentialStore(importer, store);

        try {
            credentialStore.importDocument(new FakeDocument());
        } catch (SshKeyImportCoordinator.KeystoreException expected) {
            assertEquals(1, importer.aborts.get());
            return;
        }
        throw new AssertionError("expected encrypted storage failure");
    }

    @Test public void discardRemovesEncryptedRecordAndDestroysKeystoreMaterial() throws Exception {
        FakeImporter importer = new FakeImporter();
        FakeEncryptedStore store = new FakeEncryptedStore();
        KeystoreBackedCredentialStore credentialStore = new KeystoreBackedCredentialStore(importer, store);
        SshKeyImportCoordinator.OpaqueCredentialRef reference = credential().credentialRef();

        credentialStore.discard(reference);

        assertSame(reference, store.deleted);
        assertSame(reference, importer.destroyed);
    }

    private static final class FakeDocument implements SshKeyImportCoordinator.SshKeyDocument {}

    private static final class FakeImporter implements KeystoreBackedCredentialStore.KeystoreDocumentImporter {
        final AtomicInteger aborts = new AtomicInteger();
        final FakePreparedCredential prepared = new FakePreparedCredential(credential());
        SshKeyImportCoordinator.OpaqueCredentialRef destroyed;

        @Override public KeystoreBackedCredentialStore.PreparedCredential prepare(
            SshKeyImportCoordinator.SshKeyDocument document
        ) {
            return prepared;
        }

        @Override public void abort(KeystoreBackedCredentialStore.PreparedCredential credential) {
            aborts.incrementAndGet();
        }

        @Override public void destroy(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef) {
            destroyed = credentialRef;
        }
    }

    private static final class FakeEncryptedStore implements KeystoreBackedCredentialStore.AppPrivateEncryptedBlobStore {
        boolean failReplace;
        KeystoreBackedCredentialStore.PreparedCredential replaced;
        SshKeyImportCoordinator.OpaqueCredentialRef deleted;

        @Override public void replaceAtomically(KeystoreBackedCredentialStore.PreparedCredential credential)
            throws SshKeyImportCoordinator.KeystoreException {
            if (failReplace) throw new SshKeyImportCoordinator.KeystoreException();
            replaced = credential;
        }

        @Override public void delete(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef) {
            deleted = credentialRef;
        }
    }

    private static final class FakePreparedCredential implements KeystoreBackedCredentialStore.PreparedCredential {
        private final SshKeyImportCoordinator.ImportedSshCredential credential;

        FakePreparedCredential(SshKeyImportCoordinator.ImportedSshCredential credential) {
            this.credential = credential;
        }

        @Override public SshKeyImportCoordinator.ImportedSshCredential credential() {
            return credential;
        }
    }

    private static SshKeyImportCoordinator.ImportedSshCredential credential() {
        return new SshKeyImportCoordinator.ImportedSshCredential(
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcde"
            )
        );
    }
}
