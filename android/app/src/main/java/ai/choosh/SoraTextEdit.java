package ai.choosh;

/** A dependency-neutral UTF-16 edit ready for the Rust editor boundary. */
public final class SoraTextEdit {
    private final int startUtf16;
    private final int endUtf16;
    private final String replacement;

    public SoraTextEdit(int startUtf16, int endUtf16, String replacement) {
        this.startUtf16 = startUtf16;
        this.endUtf16 = endUtf16;
        this.replacement = replacement;
    }

    public int startUtf16() { return startUtf16; }
    public int endUtf16() { return endUtf16; }
    public String replacement() { return replacement; }
}
