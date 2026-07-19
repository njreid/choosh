package ai.choosh;

import java.util.Objects;

/**
 * Coordinates an explicit document-picker result into a keystore-backed SSH credential.
 *
 * <p>This seam deliberately has no filesystem paths, document URIs, private-key byte arrays, or
 * passphrases. Android implementations of {@link ActivityResultDocumentPicker}, {@link
 * SshKeyDocumentReader}, and {@link KeystoreCredentialStore} own those details outside this
 * headless coordinator. It does not authenticate an SSH connection.</p>
 */
public final class SshKeyImportCoordinator {
    private static final int MAX_CREDENTIAL_REF_BYTES = 128;
    private static final String SHA256_PREFIX = "SHA256:";
    private static final int SHA256_BASE64_BYTES = 43;

    private final ActivityResultDocumentPicker documentPicker;
    private final SshKeyDocumentReader documentReader;
    private final KeystoreCredentialStore credentialStore;
    private final ProfileCredentialBinding profileBinding;

    public SshKeyImportCoordinator(
        ActivityResultDocumentPicker documentPicker,
        SshKeyDocumentReader documentReader,
        KeystoreCredentialStore credentialStore,
        ProfileCredentialBinding profileBinding
    ) {
        this.documentPicker = Objects.requireNonNull(documentPicker, "documentPicker");
        this.documentReader = Objects.requireNonNull(documentReader, "documentReader");
        this.credentialStore = Objects.requireNonNull(credentialStore, "credentialStore");
        this.profileBinding = Objects.requireNonNull(profileBinding, "profileBinding");
    }

    /** Starts exactly one user-mediated private-key document selection. */
    public void requestImport(OutcomeListener listener) {
        Objects.requireNonNull(listener, "listener");
        ImportCompletion completion = new ImportCompletion(listener);
        documentPicker.launchOpenDocument(completion::completeOnce);
    }

    private final class ImportCompletion {
        private final OutcomeListener listener;
        private boolean completed;

        ImportCompletion(OutcomeListener listener) {
            this.listener = listener;
        }

        void completeOnce(DocumentPickResult result) {
            if (completed) return;
            completed = true;
            complete(result, listener);
        }
    }

    private void complete(DocumentPickResult result, OutcomeListener listener) {
        if (result == null || result.isCancelled()) {
            listener.onComplete(SshKeyImportOutcome.cancelled());
            return;
        }

        final SshKeyDocument document;
        try {
            document = documentReader.open(result.document());
        } catch (DocumentReadException exception) {
            listener.onComplete(SshKeyImportOutcome.failure(SshKeyImportCode.INVALID_DOCUMENT));
            return;
        }

        final ImportedSshCredential credential;
        try {
            credential = credentialStore.importDocument(document);
        } catch (KeystoreException exception) {
            listener.onComplete(SshKeyImportOutcome.failure(SshKeyImportCode.KEYSTORE_UNAVAILABLE));
            return;
        }

        try {
            profileBinding.replaceAtomically(credential);
        } catch (ProfileBindingException exception) {
            try {
                credentialStore.discard(credential.credentialRef());
                listener.onComplete(SshKeyImportOutcome.failure(SshKeyImportCode.PROFILE_BINDING_FAILED));
            } catch (KeystoreException cleanupException) {
                listener.onComplete(SshKeyImportOutcome.failure(SshKeyImportCode.CLEANUP_FAILED));
            }
            return;
        }

        listener.onComplete(SshKeyImportOutcome.imported(credential.publicKey()));
    }

    /** Android outer adapter that launches an ACTION_OPEN_DOCUMENT-style activity result flow. */
    public interface ActivityResultDocumentPicker {
        void launchOpenDocument(DocumentPickCallback callback);
    }

    /** Result callback from a user-mediated document picker. */
    public interface DocumentPickCallback {
        void onResult(DocumentPickResult result);
    }

    /** Opaque selected-document handle. It intentionally exposes neither a URI nor bytes. */
    public interface SelectedDocument {}

    /** Opaque opened key document. It intentionally exposes no raw key material. */
    public interface SshKeyDocument {}

    /** Opens one selected document inside the platform adapter boundary. */
    public interface SshKeyDocumentReader {
        SshKeyDocument open(SelectedDocument document) throws DocumentReadException;
    }

    /** Stores a parsed key in private storage protected by a non-exportable Keystore key. */
    public interface KeystoreCredentialStore {
        ImportedSshCredential importDocument(SshKeyDocument document) throws KeystoreException;

        void discard(OpaqueCredentialRef credentialRef) throws KeystoreException;
    }

    /**
     * Atomically replaces an opaque credential binding in a profile-owned durable store.
     *
     * <p>If this method throws, the previous profile binding MUST remain intact. The
     * coordinator then discards only the newly imported credential.</p>
     */
    public interface ProfileCredentialBinding {
        void replaceAtomically(ImportedSshCredential credential) throws ProfileBindingException;
    }

    /** Receives one typed result suitable for UI projection without inspecting secrets. */
    public interface OutcomeListener {
        void onComplete(SshKeyImportOutcome outcome);
    }

    /** Selection result; selected documents are available only after explicit user action. */
    public static final class DocumentPickResult {
        private final SelectedDocument document;

        private DocumentPickResult(SelectedDocument document) {
            this.document = document;
        }

        public static DocumentPickResult selected(SelectedDocument document) {
            return new DocumentPickResult(Objects.requireNonNull(document, "document"));
        }

        public static DocumentPickResult cancelled() {
            return new DocumentPickResult(null);
        }

        public boolean isCancelled() {
            return document == null;
        }

        private SelectedDocument document() {
            return Objects.requireNonNull(document, "selected document");
        }
    }

    /** Store-local credential handle compatible with the Rust {@code CredentialRef} contract. */
    public static final class OpaqueCredentialRef {
        private final String value;

        public OpaqueCredentialRef(String value) {
            if (!isValidCredentialRef(value)) throw new IllegalArgumentException("invalid credential reference");
            this.value = value;
        }

        /** Intended only for the credential-store adapter. */
        public String valueForCredentialStore() {
            return value;
        }

        @Override public String toString() {
            return "OpaqueCredentialRef(REDACTED)";
        }
    }

    /** Public key types supported by the import boundary. */
    public enum SshPublicKeyAlgorithm { ED25519, ECDSA, RSA }

    /** Non-secret metadata shown to the user before a profile binding is replaced. */
    public static final class PublicKeyMetadata {
        private final SshPublicKeyAlgorithm algorithm;
        private final String sha256Fingerprint;

        public PublicKeyMetadata(SshPublicKeyAlgorithm algorithm, String sha256Fingerprint) {
            this.algorithm = Objects.requireNonNull(algorithm, "algorithm");
            if (!isCanonicalSha256Fingerprint(sha256Fingerprint)) {
                throw new IllegalArgumentException("invalid public key fingerprint");
            }
            this.sha256Fingerprint = sha256Fingerprint;
        }

        public SshPublicKeyAlgorithm algorithm() {
            return algorithm;
        }

        public String sha256Fingerprint() {
            return sha256Fingerprint;
        }
    }

    /** Result returned by a keystore adapter without exposing raw key material. */
    public static final class ImportedSshCredential {
        private final OpaqueCredentialRef credentialRef;
        private final PublicKeyMetadata publicKey;

        public ImportedSshCredential(OpaqueCredentialRef credentialRef, PublicKeyMetadata publicKey) {
            this.credentialRef = Objects.requireNonNull(credentialRef, "credentialRef");
            this.publicKey = Objects.requireNonNull(publicKey, "publicKey");
        }

        public OpaqueCredentialRef credentialRef() {
            return credentialRef;
        }

        public PublicKeyMetadata publicKey() {
            return publicKey;
        }
    }

    /** Stable UI-safe outcomes. No outcome contains the opaque credential reference. */
    public static final class SshKeyImportOutcome {
        private final SshKeyImportCode code;
        private final PublicKeyMetadata publicKey;

        private SshKeyImportOutcome(SshKeyImportCode code, PublicKeyMetadata publicKey) {
            this.code = code;
            this.publicKey = publicKey;
        }

        public static SshKeyImportOutcome cancelled() {
            return new SshKeyImportOutcome(SshKeyImportCode.CANCELLED, null);
        }

        public static SshKeyImportOutcome failure(SshKeyImportCode code) {
            if (code == SshKeyImportCode.IMPORTED || code == SshKeyImportCode.CANCELLED) {
                throw new IllegalArgumentException("failure requires a failure code");
            }
            return new SshKeyImportOutcome(code, null);
        }

        public static SshKeyImportOutcome imported(PublicKeyMetadata publicKey) {
            return new SshKeyImportOutcome(SshKeyImportCode.IMPORTED, Objects.requireNonNull(publicKey, "publicKey"));
        }

        public SshKeyImportCode code() {
            return code;
        }

        public PublicKeyMetadata publicKey() {
            return publicKey;
        }
    }

    public enum SshKeyImportCode {
        IMPORTED,
        CANCELLED,
        INVALID_DOCUMENT,
        KEYSTORE_UNAVAILABLE,
        PROFILE_BINDING_FAILED,
        CLEANUP_FAILED
    }

    public static final class DocumentReadException extends Exception {
        public DocumentReadException() { super(); }
    }

    public static final class KeystoreException extends Exception {
        public KeystoreException() { super(); }
    }

    public static final class ProfileBindingException extends Exception {
        public ProfileBindingException() { super(); }
    }

    private static boolean isValidCredentialRef(String value) {
        if (value == null || value.isEmpty() || value.length() > MAX_CREDENTIAL_REF_BYTES) return false;
        for (int index = 0; index < value.length(); index += 1) {
            char character = value.charAt(index);
            if (!(isAsciiLetterOrDigit(character) || character == '-' || character == '_')) return false;
        }
        return true;
    }

    private static boolean isCanonicalSha256Fingerprint(String value) {
        if (value == null || !value.startsWith(SHA256_PREFIX)
            || value.length() != SHA256_PREFIX.length() + SHA256_BASE64_BYTES) return false;
        for (int index = SHA256_PREFIX.length(); index < value.length(); index += 1) {
            char character = value.charAt(index);
            if (!(isAsciiLetterOrDigit(character) || character == '+' || character == '/')) return false;
        }
        return true;
    }

    private static boolean isAsciiLetterOrDigit(char character) {
        return (character >= 'a' && character <= 'z')
            || (character >= 'A' && character <= 'Z')
            || (character >= '0' && character <= '9');
    }
}
