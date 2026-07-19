package ai.choosh;

import io.github.rosemoe.sora.event.ContentChangeEvent;

/**
 * Translates only incremental Sora events; full set-text projections stay outside
 * this boundary so an adapter-owned suppression token can consume them first.
 */
public final class SoraContentChangeTranslator {
    private SoraContentChangeTranslator() { }

    public static SoraTextEdit translate(ContentChangeEvent event) {
        if (event == null || event.getChangeStart() == null || event.getChangeEnd() == null
                || event.getChangedText() == null) {
            throw new IllegalArgumentException("invalid_sora_content_event");
        }
        int start = event.getChangeStart().index;
        int end = event.getChangeEnd().index;
        String changedText = event.getChangedText().toString();
        if (start < 0 || end < start || end - start != changedText.length()) {
            throw new IllegalArgumentException("invalid_sora_content_event");
        }
        if (event.getAction() == ContentChangeEvent.ACTION_INSERT) {
            return new SoraTextEdit(start, start, changedText);
        }
        if (event.getAction() == ContentChangeEvent.ACTION_DELETE) {
            return new SoraTextEdit(start, end, "");
        }
        throw new IllegalArgumentException("unsupported_sora_content_action");
    }
}
