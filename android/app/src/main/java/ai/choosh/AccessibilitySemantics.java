package ai.choosh;

/** Stable, localization-independent semantics used by accessibility services and tests. */
public final class AccessibilitySemantics {
    public static final String HEADING = "choosh_heading";
    public static final String PROFILE_FIELD = "choosh_profile_field";
    public static final String CONNECT_ACTION = "choosh_connect_action";
    public static final String STATUS = "choosh_connection_status";

    private AccessibilitySemantics() { }

    public static boolean isStableId(String value) {
        return value != null && value.matches("choosh_[a-z_]+");
    }
}
