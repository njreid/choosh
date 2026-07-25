package ai.choosh;

import android.app.Activity;
import android.app.Instrumentation;
import android.os.Bundle;

/** Emits only the disposable fixture's public Android-Keystore identity. */
public final class DisposableHostKeyInstrumentation extends Instrumentation {
    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    @Override public void onStart() {
        Bundle evidence = new Bundle();
        try {
            DisposableHostKeystoreIdentity identity = DisposableHostKeystoreIdentity.open();
            evidence.putString("fixture_authorized_key", identity.authorizedKeyLine());
            evidence.putString("fixture_identity", "android-keystore-ed25519-public-only");
            finish(Activity.RESULT_OK, evidence);
        } catch (Exception failure) {
            // Keep provider/alias detail out of host automation logs.
            evidence.putString("fixture_identity", "android-keystore-unavailable");
            finish(Activity.RESULT_CANCELED, evidence);
        }
    }
}
