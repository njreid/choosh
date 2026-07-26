package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.assertFalse;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.List;
import org.junit.Test;

/** Deterministic artifact-selection and staging proof with no network, file, or install authority. */
public final class ReleaseUpdatePlannerTest {
    private static final String CERT = "1111111111111111111111111111111111111111111111111111111111111111";

    @Test public void selects_verifies_and_returns_write_free_staging_plan() throws Exception {
        byte[] apk = "fixture-apk-0.2.0".getBytes(StandardCharsets.US_ASCII);
        ReleaseUpdatePlanner planner = new ReleaseUpdatePlanner(bytes -> CERT);
        ReleaseUpdatePlanner.Candidate candidate = planner.select(installed(), index());
        ReleaseUpdatePlanner.StagingPlan plan = planner.verify(installed(), candidate, apk,
            (sha(apk) + "  choosh-0.2.0.apk\n").getBytes(StandardCharsets.US_ASCII),
            new ReleaseUpdatePlanner.SignerEvidence("choosh-0.2.0.apk", CERT));

        assertEquals("choosh-0.2.0.apk", candidate.apkAsset());
        assertEquals("choosh-0.2.0.sha256", candidate.checksumAsset());
        assertEquals(sha(apk), plan.sha256());
        assertArrayEquals(apk, plan.copyApkBytesForStaging());
    }

    @Test public void checksum_or_certificate_mismatch_never_yields_staging_plan() throws Exception {
        byte[] apk = "fixture-apk-0.2.0".getBytes(StandardCharsets.US_ASCII);
        ReleaseUpdatePlanner.Candidate candidate = new ReleaseUpdatePlanner(new Certificate(CERT)).select(installed(), index());
        assertThrows(ReleaseUpdatePlanner.UpdateException.class, () -> new ReleaseUpdatePlanner(new Certificate(CERT))
            .verify(installed(), candidate, apk, ("00" + sha(apk).substring(2) + "  choosh-0.2.0.apk\n")
                .getBytes(StandardCharsets.US_ASCII), new ReleaseUpdatePlanner.SignerEvidence("choosh-0.2.0.apk", CERT)));
        assertThrows(ReleaseUpdatePlanner.UpdateException.class, () -> new ReleaseUpdatePlanner(new Certificate(
            "2222222222222222222222222222222222222222222222222222222222222222"))
            .verify(installed(), candidate, apk, (sha(apk) + "  choosh-0.2.0.apk\n")
                .getBytes(StandardCharsets.US_ASCII), new ReleaseUpdatePlanner.SignerEvidence("choosh-0.2.0.apk", CERT)));
    }

    @Test public void ambiguous_or_nonmonotonic_indexes_are_rejected_before_download() {
        ReleaseUpdatePlanner planner = new ReleaseUpdatePlanner(new Certificate(CERT));
        assertThrows(ReleaseUpdatePlanner.UpdateException.class, () -> planner.select(installed(), new ReleaseUpdatePlanner.ReleaseIndex(List.of(
            release("v0.2.0", "choosh-0.2.0.apk", "choosh-0.2.0.apk", "choosh-0.2.0.sha256", "choosh-0.2.0.apk.signer.json")
        ))));
        assertThrows(ReleaseUpdatePlanner.UpdateException.class, () -> planner.select(installed(), new ReleaseUpdatePlanner.ReleaseIndex(List.of(
            release("v0.1.0", "choosh-0.1.0.apk", "choosh-0.1.0.sha256", "choosh-0.1.0.apk.signer.json")
        ))));
    }

    private static ReleaseUpdatePlanner.Installed installed() { return new ReleaseUpdatePlanner.Installed("0.1.0", CERT); }
    private static ReleaseUpdatePlanner.ReleaseIndex index() { return new ReleaseUpdatePlanner.ReleaseIndex(List.of(
        release("v0.1.0", "choosh-0.1.0.apk", "choosh-0.1.0.sha256", "choosh-0.1.0.apk.signer.json"),
        release("v0.2.0", "choosh-0.2.0.apk", "choosh-0.2.0.sha256", "choosh-0.2.0.apk.signer.json")
    )); }
    private static ReleaseUpdatePlanner.Release release(String tag, String... assets) { return new ReleaseUpdatePlanner.Release(tag, false, false, List.of(assets)); }
    private static String sha(byte[] bytes) throws Exception { StringBuilder text = new StringBuilder(); for (byte value : MessageDigest.getInstance("SHA-256").digest(bytes)) text.append(String.format("%02x", value)); return text.toString(); }
    private static final class Certificate implements ReleaseUpdatePlanner.ApkCertificateVerifier { private final String digest; Certificate(String digest) { this.digest = digest; } @Override public String certificateSha256(byte[] apk) { return digest; } }

    @Test public void hostEnvelopeContainsOnlyVerifiedReleaseFields() throws Exception {
        ReleaseUpdatePlanner planner = new ReleaseUpdatePlanner(bytes -> CERT);
        ReleaseUpdatePlanner.Candidate candidate = planner.select(installed(), index());
        byte[] apk = "fixture-apk-0.2.0".getBytes(StandardCharsets.US_ASCII);
        ReleaseUpdatePlanner.StagingPlan plan = planner.verify(installed(), candidate, apk,
            (sha(apk) + "  choosh-0.2.0.apk\n").getBytes(StandardCharsets.US_ASCII),
            new ReleaseUpdatePlanner.SignerEvidence("choosh-0.2.0.apk", CERT));
        String envelope = new String(HostDeploymentEnvelope.encode(plan), StandardCharsets.UTF_8);
        assertTrue(envelope.contains("\"schema_version\":1"));
        assertTrue(envelope.contains("\"version\":\"0.2.0\""));
        assertFalse(envelope.contains("/"));
    }
}
