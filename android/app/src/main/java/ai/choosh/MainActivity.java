package ai.choosh;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

public final class MainActivity extends Activity {
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        TextView content = new TextView(this);
        content.setText(getString(R.string.app_name));
        content.setContentDescription(getString(R.string.app_name));
        setContentView(content);
    }
}
