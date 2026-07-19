package ai.choosh;

import java.util.Objects;

/**
 * Transactional seam between an imported key document and app-private encrypted storage.
 *
 * <p>Implementations of the injected importer may access private key material only inside the
 * Android/Keystore boundary. This class and its public values carry opaque prepared records,
 * store-local references, and public key metadata only; they have no byte-array, path, URI, or
 * passphrase API.</p>
 */
public final class KeystoreBackedCredentialStore
    implements SshKeyImportCoordinator.KeystoreCredentialStore {
    private final KeystoreDocumentImporter importer;
    private final AppPrivateEncryptedBlobStore encryptedStore;

    public KeystoreBackedCredentialStore(
        KeystoreDocumentImporter importer,
        AppPrivateEncryptedBlobStore encryptedStore
    ) {
        this.importer = Objects.requireNonNull(importer, "importer");
        this.encryptedStore = Objects.requireNonNull(encryptedStore, "encryptedStore");
    }

    @Override public SshKeyImportCoordinator.ImportedSshCredential importDocument(
        SshKeyImportCoordinator.SshKeyDocument document
    ) throws SshKeyImportCoordinator.KeystoreException {
        PreparedCredential prepared = importer.prepare(Objects.requireNonNull(document, "document"));
        try {
            encryptedStore.replaceAtomically(prepared);
        } catch (SshKeyImportCoordinator.KeystoreException failure) {
            try {
                importer.abort(prepared);
            } catch (SshKeyImportCoordinator.KeystoreException ignored) {
                // The externally visible failure remains a stable keystore failure.
            }
            throw failure;
        }
        return prepared.credential();
    }

    @Override public void discard(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef)
        throws SshKeyImportCoordinator.KeystoreException {
        Objects.requireNonNull(credentialRef, "credentialRef");
        SshKeyImportCoordinator.KeystoreException failure = null;
        try {
            encryptedStore.delete(credentialRef);
        } catch (SshKeyImportCoordinator.KeystoreException exception) {
            failure = exception;
        }
        try {
            importer.destroy(credentialRef);
        } catch (SshKeyImportCoordinator.KeystoreException exception) {
            failure = exception;
        }
        if (failure != null) throw failure;
    }

    /**
     * Android Keystore/document adapter. Prepared records are opaque and must be abandoned on a
     * failed app-private write.
     */
    public interface KeystoreDocumentImporter {
        PreparedCredential prepare(SshKeyImportCoordinator.SshKeyDocument document)
            throws SshKeyImportCoordinator.KeystoreException;

        void abort(PreparedCredential prepared) throws SshKeyImportCoordinator.KeystoreException;

        void destroy(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef)
            throws SshKeyImportCoordinator.KeystoreException;
    }

    /**
     * App-private encrypted-record store. Replacement is atomic: a failure leaves no usable new
     * record, and deletion removes the record addressed by exactly one opaque credential handle.
     */
    public interface AppPrivateEncryptedBlobStore {
        void replaceAtomically(PreparedCredential prepared) throws SshKeyImportCoordinator.KeystoreException;

        void delete(SshKeyImportCoordinator.OpaqueCredentialRef credentialRef)
            throws SshKeyImportCoordinator.KeystoreException;
    }

    /** Opaque encrypted record plus public metadata; it deliberately has no material accessors. */
    public interface PreparedCredential {
        SshKeyImportCoordinator.ImportedSshCredential credential();
    }
}
