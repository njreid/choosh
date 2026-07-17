package ai.choosh;

import android.app.Instrumentation;
import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.TextView;

public final class SmokeInstrumentation extends Instrumentation {
    @Override public void onCreate(Bundle arguments) { super.onCreate(arguments); start(); }
    @Override public void onStart() {
        Bundle evidence = new Bundle();
        String expectedPackage = BuildIdentity.packageName();
        require("ai.choosh".equals(expectedPackage), "build identity changed");
        require(expectedPackage.equals(getTargetContext().getPackageName()), "target package mismatch");

        Intent launch = getTargetContext().getPackageManager().getLaunchIntentForPackage(expectedPackage);
        require(launch != null, "launcher intent missing");
        require(launch.getComponent() != null, "launcher component missing");
        require(expectedPackage.equals(launch.getComponent().getPackageName()), "launcher package mismatch");
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        Activity activity = startActivitySync(launch);
        waitForIdleSync();
        require(activity instanceof MainActivity, "unexpected launcher activity");
        require(!activity.isFinishing() && !activity.isDestroyed(), "activity did not reach active state");
        require(activity.findViewById(android.R.id.content) != null, "content root missing");
        require(activity.findViewById(android.R.id.content).getRootView().isAttachedToWindow(), "content not attached");

        // The activity's only content child carries the deterministic M0 label.
        TextView label = (TextView) ((android.view.ViewGroup) activity.findViewById(android.R.id.content)).getChildAt(0);
        require("Choosh".contentEquals(label.getText()), "visible application label mismatch");
        require("Choosh".contentEquals(label.getContentDescription()), "accessible label mismatch");
        runOnMainSync(activity::finish);
        waitForIdleSync();
        require(activity.isFinishing() || activity.isDestroyed(), "activity teardown was not observed");
        evidence.putString("package", expectedPackage);
        evidence.putString("activity", MainActivity.class.getName());
        evidence.putString("lifecycle", "active-then-finished");
        evidence.putString("accessibility_label", "Choosh");
        finish(Activity.RESULT_OK, evidence);
    }

    private static void require(boolean condition, String message) {
        if (!condition) { throw new AssertionError(message); }
    }
}
