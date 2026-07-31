package ai.choosh;

import android.app.Activity;
import android.os.Bundle;
import android.graphics.Typeface;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.Gravity;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

public final class MainActivity extends Activity {
    private ConnectionScreenController screen;
    private Button connect;
    private TextView status;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setGravity(Gravity.CENTER_HORIZONTAL);
        int inset = (int) (24 * getResources().getDisplayMetrics().density);
        root.setPadding(inset, inset, inset, inset);

        TextView heading = new TextView(this);
        heading.setContentDescription(AccessibilitySemantics.HEADING);
        heading.setText(R.string.app_name);
        heading.setTextSize(32);
        heading.setTypeface(getResources().getFont(R.font.choosh_terminal), Typeface.BOLD);
        root.addView(heading, new LinearLayout.LayoutParams(-1, -2));

        EditText profile = new EditText(this);
        profile.setContentDescription(AccessibilitySemantics.PROFILE_FIELD);
        profile.setHint(R.string.profile_hint);
        profile.setSingleLine(true);
        profile.setInputType(android.text.InputType.TYPE_CLASS_TEXT);
        root.addView(profile, new LinearLayout.LayoutParams(-1, -2));

        connect = new Button(this);
        connect.setContentDescription(AccessibilitySemantics.CONNECT_ACTION);
        connect.setText(R.string.connect);
        root.addView(connect, new LinearLayout.LayoutParams(-1, -2));

        status = new TextView(this);
        status.setContentDescription(AccessibilitySemantics.STATUS);
        status.setAccessibilityLiveRegion(android.view.View.ACCESSIBILITY_LIVE_REGION_POLITE);
        status.setTypeface(getResources().getFont(R.font.choosh_ui));
        status.setPadding(0, inset, 0, 0);
        root.addView(status, new LinearLayout.LayoutParams(-1, -2));
        setContentView(root);

        screen = new ConnectionScreenController(unavailableCoordinator(), this::render);
        profile.addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence value, int start, int count, int after) { }
            @Override public void onTextChanged(CharSequence value, int start, int before, int count) {
                screen.selectProfile(value.toString());
            }
            @Override public void afterTextChanged(Editable value) { }
        });
        connect.setOnClickListener(view -> {
            screen.selectProfile(profile.getText().toString());
            screen.connect();
        });
        render(screen.state());
    }

    private AuthenticatedSshOperationCoordinator unavailableCoordinator() {
        // Durable profile and native connector implementations are intentionally injected here
        // when their storage/Keystore composition root is available.
        return new AuthenticatedSshOperationCoordinator(
            profileId -> { throw new AuthenticatedSshOperationCoordinator.ProfileUnavailableException(); },
            (request, callback) -> { throw new AuthenticatedSshOperationCoordinator.SshTransportException(); }
        );
    }

    private void render(ConnectionScreenController.State value) {
        runOnUiThread(() -> {
            connect.setEnabled(value.canConnect() && !value.isBusy());
            int message;
            switch (value.phase()) {
                case INVALID_PROFILE: message = R.string.status_invalid_profile; break;
                case READY: message = R.string.status_ready; break;
                case CONNECTING: message = R.string.status_connecting; break;
                case CONNECTED: message = R.string.status_connected; break;
                case PROFILE_UNAVAILABLE: message = R.string.status_profile_unavailable; break;
                case HOST_KEY_REJECTED: message = R.string.status_host_key_rejected; break;
                case AUTHENTICATION_FAILED: message = R.string.status_authentication_failed; break;
                case TRANSPORT_UNAVAILABLE: message = R.string.status_transport_unavailable; break;
                case AWAITING_PROFILE:
                default: message = R.string.status_awaiting_profile; break;
            }
            status.setText(message);
        });
    }
}
