package ai.choosh;

import android.app.Instrumentation;
import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.view.ViewGroup;

import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicInteger;

import io.github.rosemoe.sora.event.ContentChangeEvent;
import io.github.rosemoe.sora.event.SubscriptionReceipt;
import io.github.rosemoe.sora.text.CharPosition;
import io.github.rosemoe.sora.widget.CodeEditor;

public final class SmokeInstrumentation extends Instrumentation {
    @Override public void onCreate(Bundle arguments) { super.onCreate(arguments); start(); }
    @Override public void onStart() {
        Bundle evidence = new Bundle();
        verifyJniBridgeLibrary(evidence);
        // Sora constructs Android gesture handlers, so both widget fixtures
        // must run on the main looper rather than the instrumentation thread.
        runOnMainSync(() -> {
            verifySoraLifecycle(evidence);
            verifySoraEventTranslation(evidence);
        });
        verifyControlledConnectorFixture(evidence);
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

        ViewGroup content = (ViewGroup) activity.findViewById(android.R.id.content);
        require(content.getChildCount() == 1, "unexpected content hierarchy");
        require(content.getChildAt(0) instanceof LinearLayout, "connection layout missing");
        LinearLayout screen = (LinearLayout) content.getChildAt(0);
        require(screen.getChildCount() == 4, "connection controls missing");
        TextView heading = (TextView) screen.getChildAt(0);
        EditText profile = (EditText) screen.getChildAt(1);
        Button connect = (Button) screen.getChildAt(2);
        TextView status = (TextView) screen.getChildAt(3);
        require("Choosh".contentEquals(heading.getText()), "visible application label mismatch");
        require("Profile ID".contentEquals(profile.getHint()), "profile input label mismatch");
        require("Connect".contentEquals(connect.getText()), "connect label mismatch");
        runOnMainSync(() -> profile.setText("fixture_profile"));
        waitForIdleSync();
        require(connect.isEnabled(), "valid profile did not enable connection action");
        runOnMainSync(connect::performClick);
        waitForIdleSync();
        require("This saved profile is unavailable.".contentEquals(status.getText()),
                "default unavailable connector state mismatch");
        runOnMainSync(activity::finish);
        waitForIdleSync();
        require(activity.isFinishing() || activity.isDestroyed(), "activity teardown was not observed");
        evidence.putString("package", expectedPackage);
        evidence.putString("activity", MainActivity.class.getName());
        evidence.putString("lifecycle", "active-then-finished");
        evidence.putString("connection_screen", "labels-and-unavailable-profile");
        evidence.putString("controlled_connector", "planned-native-git-status-ready");
        finish(Activity.RESULT_OK, evidence);
    }

    /** Loads the packaged JNI bridge and resolves its nested-class ABI entry point on-device. */
    private static void verifyJniBridgeLibrary(Bundle evidence) {
        require(new RustNativeConnectorJni.JniPlanBridge().abiVersion() == 3,
                "native bridge ABI mismatch");
        evidence.putString("jni_bridge", "nested-class-abi-v3-resolved");
    }

    private void verifySoraLifecycle(Bundle evidence) {
        AtomicInteger events = new AtomicInteger();
        CodeEditor editor = new CodeEditor(getTargetContext());
        SubscriptionReceipt<ContentChangeEvent> receipt = editor.subscribeEvent(
                ContentChangeEvent.class,
                (event, unsubscribe) -> {
                    require(event.getChangeStart() != null, "Sora change start missing");
                    require(event.getChangeEnd() != null, "Sora change end missing");
                    require(event.getChangedText() != null, "Sora changed text missing");
                    events.incrementAndGet();
                });
        editor.setText("M0");
        require("M0".contentEquals(editor.getText()), "Sora text projection mismatch");
        require(events.get() == 1, "Sora setText must publish one change event");
        receipt.unsubscribe();
        editor.release();
        evidence.putString("sora", "0.24.6:setText-event-and-release");
    }

    private void verifySoraEventTranslation(Bundle evidence) {
        CodeEditor editor = new CodeEditor(getTargetContext());
        SoraTextEdit insert = SoraContentChangeTranslator.translate(new ContentChangeEvent(
                editor,
                ContentChangeEvent.ACTION_INSERT,
                position(1),
                position(3),
                "😀",
                false));
        require(insert.startUtf16() == 1 && insert.endUtf16() == 1,
                "Sora insert must use its pre-insert range");
        require("😀".equals(insert.replacement()), "Sora insert replacement mismatch");

        SoraTextEdit delete = SoraContentChangeTranslator.translate(new ContentChangeEvent(
                editor,
                ContentChangeEvent.ACTION_DELETE,
                position(1),
                position(3),
                "😀",
                false));
        require(delete.startUtf16() == 1 && delete.endUtf16() == 3,
                "Sora delete range mismatch");
        require(delete.replacement().isEmpty(), "Sora delete must have an empty replacement");
        expectTranslationFailure(() -> SoraContentChangeTranslator.translate(new ContentChangeEvent(
                editor,
                ContentChangeEvent.ACTION_SET_NEW_TEXT,
                position(0),
                position(2),
                "M0",
                false)));
        editor.release();
        evidence.putString("sora_translation", "insert-delete-utf16-and-projection-rejection");
    }

    private static CharPosition position(int index) {
        CharPosition position = new CharPosition();
        position.index = index;
        return position;
    }

    private static void expectTranslationFailure(Runnable action) {
        try {
            action.run();
            throw new AssertionError("Sora full projection was translated as an incremental edit");
        } catch (IllegalArgumentException expected) {
            require("unsupported_sora_content_action".equals(expected.getMessage()),
                    "unexpected Sora translation failure");
        }
    }

    private void verifyControlledConnectorFixture(Bundle evidence) {
        ControlledBridge bridge = new ControlledBridge();
        ControlledRuntime runtime = new ControlledRuntime();
        ControlledSession session = new ControlledSession();
        AndroidGitStatusComposition composition = AndroidGitStatusComposition.fromNativeRuntime(
                ignored -> request(), () -> 7, bridge, runtime,
                (plan, callback) -> callback.onComplete(
                        NativeAuthenticatedSshConnector.NativeOpenResult.connected(session)),
                () -> GitStatusRpc.request(
                        new GitStatusRpc.WorkspaceId("00000000-0000-4000-8000-000000000001"),
                        new GitStatusRpc.RequestId("00000000-0000-4000-8000-000000000002")));
        ControlledListener listener = new ControlledListener();
        composition.refresh(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), listener);
        require(bridge.generation == 7 && bridge.cancels == 1, "planned connector lifecycle mismatch");
        require(runtime.releases == 1, "runtime lease was not released");
        require(listener.failure == null && listener.state != null,
                "controlled connector did not reach Git status");
        require(listener.state.phase() == GitStatusController.Phase.READY,
                "controlled Git status was not ready");
        require(session.request != null && new String(session.request, StandardCharsets.UTF_8)
                .contains("\"method\":\"git.status\""), "fixed Git RPC was not emitted");
        evidence.putString("controlled_connector_lifecycle", "lease-released-fixed-git-status");
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request() {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
                new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
                new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
                new ProfileConnectionMetadataSource.SshUsername("fixture_user"),
                new ProfileConnectionMetadataSource.KnownHost(
                        ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                        "SHA256:0123456789012345678901234567890123456789012"),
                new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_fixture_42"),
                new SshKeyImportCoordinator.PublicKeyMetadata(
                        SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                        "SHA256:0123456789012345678901234567890123456789012"));
    }

    private static final class ControlledBridge implements RustNativeConnectorJni.NativePlanBridge {
        int generation;
        int cancels;
        @Override public int abiVersion() { return 3; }
        @Override public long beginAuthenticatedPlan(int generation, RustNativeConnectorJni.NativeHandles handles) {
            this.generation = generation;
            return 29;
        }
        @Override public int openAuthenticatedPlan(int generation, long plan) { return 5; }
        @Override public int cancelAuthenticatedPlan(int generation, long plan) { cancels++; return 0; }
    }

    private static final class ControlledRuntime implements RustNativeConnectorJni.NativeRuntime {
        int releases;
        @Override public RustNativeConnectorJni.NativeLease acquire(
            NativeAuthenticatedSshConnector.NativeConnectionInput input
        ) {
            return new RustNativeConnectorJni.NativeLease(
                    new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7),
                    () -> releases++);
        }
    }

    private static final class ControlledSession implements NativeAuthenticatedSshConnector.NativeSession {
        byte[] request;
        @Override public void executeRpc(byte[] request, NativeAuthenticatedSshConnector.NativeRpcCallback callback) {
            this.request = request.clone();
            callback.onComplete(new NativeAuthenticatedSshConnector.NativeRpcResult(
                    "{\"id\":\"00000000-0000-4000-8000-000000000002\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"00000000-0000-4000-8000-000000000001\",\"entries\":[]}}"
                            .getBytes(StandardCharsets.UTF_8)));
        }
    }

    private static final class ControlledListener implements AndroidGitStatusComposition.Listener {
        AuthenticatedSshOperationCoordinator.OpenCode failure;
        GitStatusController.State state;
        @Override public void onConnectionFailure(AuthenticatedSshOperationCoordinator.OpenCode value) { failure = value; }
        @Override public void onGitStatusController(GitStatusController controller) { }
        @Override public void onGitStatusState(GitStatusController.State value) { state = value; }
    }

    private static void require(boolean condition, String message) {
        if (!condition) { throw new AssertionError(message); }
    }
}
