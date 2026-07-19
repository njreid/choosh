package ai.choosh;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;

import java.util.Objects;

/**
 * Android outer adapter for the user-mediated {@link Intent#ACTION_OPEN_DOCUMENT} flow.
 *
 * <p>The injected launcher owns Activity Result API wiring. This adapter asks only for one
 * temporary read grant and converts a successful result into an opaque {@code SelectedDocument};
 * it does not persist URI permissions, inspect paths, or open document contents.</p>
 */
public final class AndroidOpenDocumentPicker
    implements SshKeyImportCoordinator.ActivityResultDocumentPicker {
    private final OpenDocumentLauncher launcher;

    public AndroidOpenDocumentPicker(OpenDocumentLauncher launcher) {
        this.launcher = Objects.requireNonNull(launcher, "launcher");
    }

    @Override public void launchOpenDocument(SshKeyImportCoordinator.DocumentPickCallback callback) {
        Objects.requireNonNull(callback, "callback");
        launcher.launch(openDocumentIntent(), (resultCode, data) -> {
            Uri uri = data == null ? null : data.getData();
            if (resultCode != Activity.RESULT_OK || uri == null) {
                callback.onResult(SshKeyImportCoordinator.DocumentPickResult.cancelled());
                return;
            }
            callback.onResult(SshKeyImportCoordinator.DocumentPickResult.selected(new AndroidSelectedDocument(uri)));
        });
    }

    /**
     * Builds the least-privilege document-picker request for key import.
     *
     * <p>Private keys have no reliable Android MIME type, so the picker accepts all types. It
     * deliberately omits persistable-write grants and does not request storage permissions.</p>
     */
    static Intent openDocumentIntent() {
        RequestPolicy policy = requestPolicy();
        Intent intent = new Intent(policy.action()).setType("*/*");
        if (policy.openableOnly()) intent.addCategory(Intent.CATEGORY_OPENABLE);
        if (policy.readGrant()) intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        return intent;
    }

    /** Pure request policy kept separate so the least-privilege contract has a JVM test. */
    static RequestPolicy requestPolicy() {
        return new RequestPolicy(Intent.ACTION_OPEN_DOCUMENT, true, true, false, false);
    }

    /** Read-only document-picker policy; it contains no URI, path, or storage permission. */
    static final class RequestPolicy {
        private final String action;
        private final boolean openableOnly;
        private final boolean readGrant;
        private final boolean writeGrant;
        private final boolean persistableGrant;

        RequestPolicy(
            String action,
            boolean openableOnly,
            boolean readGrant,
            boolean writeGrant,
            boolean persistableGrant
        ) {
            this.action = action;
            this.openableOnly = openableOnly;
            this.readGrant = readGrant;
            this.writeGrant = writeGrant;
            this.persistableGrant = persistableGrant;
        }

        String action() { return action; }
        boolean openableOnly() { return openableOnly; }
        boolean readGrant() { return readGrant; }
        boolean writeGrant() { return writeGrant; }
        boolean persistableGrant() { return persistableGrant; }
    }

    /** Injected bridge to a registered Android Activity Result launcher. */
    public interface OpenDocumentLauncher {
        void launch(Intent intent, ResultCallback callback);
    }

    /** Normalized result from the injected Activity Result launcher. */
    public interface ResultCallback {
        void onResult(int resultCode, Intent data);
    }

    /** Opaque Android document handle consumable only by a package-private document reader. */
    static final class AndroidSelectedDocument implements SshKeyImportCoordinator.SelectedDocument {
        private final Uri uri;

        AndroidSelectedDocument(Uri uri) {
            this.uri = Objects.requireNonNull(uri, "uri");
        }

        Uri uriForDocumentReader() {
            return uri;
        }
    }
}
