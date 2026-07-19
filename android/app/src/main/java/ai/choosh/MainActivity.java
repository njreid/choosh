package ai.choosh;

import android.app.Activity;
import android.os.Bundle;

import io.github.rosemoe.sora.widget.CodeEditor;

public final class MainActivity extends Activity {
    private CodeEditor editor;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        editor = new CodeEditor(this);
        editor.setText(getString(R.string.app_name));
        editor.setTypefaceText(getResources().getFont(R.font.choosh_terminal));
        editor.setContentDescription(getString(R.string.app_name));
        setContentView(editor);
    }

    @Override protected void onDestroy() {
        if (editor != null) {
            editor.release();
            editor = null;
        }
        super.onDestroy();
    }
}
