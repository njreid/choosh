package ai.choosh;

/** Builds the bounded schema-1 host update envelope; it has no path or command authority. */
public final class HostDeploymentEnvelope {
    private HostDeploymentEnvelope() {}

    public static byte[] encode(ReleaseUpdatePlanner.StagingPlan plan) {
        if (plan == null) throw new IllegalArgumentException("missing_staging_plan");
        byte[] bytes = plan.copyApkBytesForStaging();
        StringBuilder artifact = new StringBuilder();
        artifact.append('[');
        for (int i = 0; i < bytes.length; i++) {
            if (i > 0) artifact.append(',');
            artifact.append(bytes[i] & 0xff);
        }
        artifact.append(']');
        return ("{\"schema_version\":1,\"version\":\"" + plan.version()
            + "\",\"sha256\":\"" + plan.sha256() + "\",\"artifact\":" + artifact + "}")
            .getBytes(java.nio.charset.StandardCharsets.UTF_8);
    }
}
