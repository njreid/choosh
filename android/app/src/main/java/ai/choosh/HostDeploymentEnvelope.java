package ai.choosh;

import java.nio.charset.StandardCharsets;
import java.util.Base64;

/** Builds the bounded schema-1 host update envelope; it has no path or command authority. */
public final class HostDeploymentEnvelope {
    private HostDeploymentEnvelope() {}

    /**
     * Encodes one verified staging plan for the host deployment boundary.
     *
     * <p>The artifact travels as canonical unpadded base64url, matching the encoding the
     * rest of the wire protocol uses for byte-preserving fields. A decimal array would
     * expand a multi-megabyte release roughly fourfold and cost the host one JSON value
     * per byte to decode.
     */
    public static byte[] encode(ReleaseUpdatePlanner.StagingPlan plan) {
        if (plan == null) throw new IllegalArgumentException("missing_staging_plan");
        String artifact = Base64.getUrlEncoder().withoutPadding()
            .encodeToString(plan.copyApkBytesForStaging());
        return ("{\"schema_version\":1,\"version\":\"" + plan.version()
            + "\",\"sha256\":\"" + plan.sha256() + "\",\"artifact_b64\":\"" + artifact + "\"}")
            .getBytes(StandardCharsets.UTF_8);
    }
}
