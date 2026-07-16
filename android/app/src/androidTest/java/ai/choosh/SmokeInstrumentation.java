package ai.choosh;

import android.app.Instrumentation;
import android.content.Intent;
import android.os.Bundle;

public final class SmokeInstrumentation extends Instrumentation {
    @Override public void onCreate(Bundle arguments) { super.onCreate(arguments); start(); }
    @Override public void onStart() {
        Intent launch = getTargetContext().getPackageManager().getLaunchIntentForPackage("ai.choosh");
        if (launch == null) { finish(android.app.Activity.RESULT_CANCELED, new Bundle()); return; }
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        getTargetContext().startActivity(launch);
        finish(android.app.Activity.RESULT_OK, new Bundle());
    }
}
