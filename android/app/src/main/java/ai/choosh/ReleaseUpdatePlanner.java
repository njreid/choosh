package ai.choosh;

import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.Objects;

/**
 * Deterministic, side-effect-free planner for an Android APK update.
 *
 * <p>Release indexes and downloaded bytes are untrusted. This class selects only canonical
 * stable release asset names, verifies their bounded evidence, and returns a staging plan. It
 * cannot download, write a path, start package installation, open SSH, or construct a host
 * command; those authorities remain injected outer adapters.</p>
 */
public final class ReleaseUpdatePlanner {
    private static final int MAX_RELEASES = 100;
    private static final int MAX_ASSETS = 32;
    private static final int MAX_APK_BYTES = 256 * 1024 * 1024;
    private static final int MAX_CHECKSUM_BYTES = 256;
    private static final int MAX_SIGNER_BYTES = 4096;
    private final ApkCertificateVerifier certificates;

    public ReleaseUpdatePlanner(ApkCertificateVerifier certificates) {
        this.certificates = Objects.requireNonNull(certificates, "certificates");
    }

    /** Selects the unique newest stable release strictly newer than the installed version. */
    public Candidate select(Installed installed, ReleaseIndex index) throws UpdateException {
        Objects.requireNonNull(installed, "installed");
        Objects.requireNonNull(index, "index");
        List<Candidate> candidates = new ArrayList<>();
        for (Release release : index.releases) {
            if (release.draft || release.prerelease) continue;
            final Version version;
            try {
                version = Version.parseTag(release.tag);
            } catch (IllegalArgumentException exception) {
                throw new UpdateException();
            }
            if (version.compareTo(installed.version) <= 0) continue;
            String base = "choosh-" + version + ".apk";
            if (!release.hasExactly(base) || !release.hasExactly("choosh-" + version + ".sha256")
                || !release.hasExactly(base + ".signer.json") || release.apkCount() != 1) {
                throw new UpdateException();
            }
            candidates.add(new Candidate(version, base, "choosh-" + version + ".sha256",
                base + ".signer.json"));
        }
        if (candidates.isEmpty()) throw new UpdateException();
        candidates.sort(Comparator.comparing(candidate -> candidate.version));
        Candidate selected = candidates.get(candidates.size() - 1);
        if (candidates.size() > 1 && candidates.get(candidates.size() - 2).version.equals(selected.version)) {
            throw new UpdateException();
        }
        return selected;
    }

    /** Verifies downloaded bounded artifacts and returns a write-free staging plan. */
    public StagingPlan verify(
        Installed installed, Candidate candidate, byte[] apk, byte[] checksum,
        SignerEvidence signer
    ) throws UpdateException {
        Objects.requireNonNull(installed, "installed");
        Objects.requireNonNull(candidate, "candidate");
        requireLength(apk, 1, MAX_APK_BYTES);
        requireLength(checksum, 1, MAX_CHECKSUM_BYTES);
        Objects.requireNonNull(signer, "signer");
        if (!candidate.apk.equals(signer.apk) || !installed.certificateSha256.equals(signer.certificateSha256)) {
            throw new UpdateException();
        }
        String expected = parseChecksum(checksum, candidate.apk);
        String actual = sha256(apk);
        if (!expected.equals(actual) || !installed.certificateSha256.equals(certificates.certificateSha256(apk))) {
            throw new UpdateException();
        }
        return new StagingPlan(candidate.version, candidate.apk, actual, apk.clone());
    }

    private static void requireLength(byte[] bytes, int minimum, int maximum) throws UpdateException {
        if (bytes == null || bytes.length < minimum || bytes.length > maximum) throw new UpdateException();
    }

    private static String parseChecksum(byte[] bytes, String expectedName) throws UpdateException {
        String line = new String(bytes, java.nio.charset.StandardCharsets.US_ASCII);
        if (!line.endsWith("\n") || line.indexOf('\n') != line.length() - 1) throw new UpdateException();
        String[] fields = line.substring(0, line.length() - 1).split("  ", -1);
        if (fields.length != 2 || !isHash(fields[0]) || !expectedName.equals(fields[1])) throw new UpdateException();
        return fields[0];
    }

    private static String sha256(byte[] bytes) throws UpdateException {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256").digest(bytes);
            StringBuilder text = new StringBuilder(64);
            for (byte value : digest) text.append(String.format(Locale.ROOT, "%02x", value));
            return text.toString();
        } catch (NoSuchAlgorithmException exception) {
            throw new UpdateException();
        }
    }

    private static boolean isHash(String value) {
        return value.length() == 64 && value.matches("[0-9a-f]{64}");
    }

    /** Injected APK signature inspector. It exposes no package manager or install authority. */
    public interface ApkCertificateVerifier {
        String certificateSha256(byte[] apk) throws UpdateException;
    }

    public static final class Installed {
        private final Version version;
        private final String certificateSha256;
        public Installed(String version, String certificateSha256) {
            this.version = Version.parseInstalled(version);
            if (!isHash(certificateSha256)) throw new IllegalArgumentException("invalid certificate digest");
            this.certificateSha256 = certificateSha256;
        }
    }

    public static final class ReleaseIndex {
        private final List<Release> releases;
        public ReleaseIndex(List<Release> releases) {
            Objects.requireNonNull(releases, "releases");
            if (releases.isEmpty() || releases.size() > MAX_RELEASES) throw new IllegalArgumentException("invalid releases");
            this.releases = List.copyOf(releases);
        }
    }

    public static final class Release {
        private final String tag;
        private final boolean draft;
        private final boolean prerelease;
        private final List<String> assets;
        public Release(String tag, boolean draft, boolean prerelease, List<String> assets) {
            this.tag = requireTag(tag);
            this.draft = draft;
            this.prerelease = prerelease;
            Objects.requireNonNull(assets, "assets");
            if (assets.size() > MAX_ASSETS) throw new IllegalArgumentException("too many assets");
            for (String asset : assets) if (!isAsset(asset)) throw new IllegalArgumentException("invalid asset");
            this.assets = List.copyOf(assets);
        }
        private boolean hasExactly(String expected) {
            int count = 0;
            for (String asset : assets) if (expected.equals(asset)) count++;
            return count == 1;
        }
        private int apkCount() {
            int count = 0;
            for (String asset : assets) if (asset.matches("choosh-[0-9]+\\.[0-9]+\\.[0-9]+\\.apk")) count++;
            return count;
        }
    }

    public static final class SignerEvidence {
        private final String apk;
        private final String certificateSha256;
        /** A bounded JSON parser at the download edge must construct this only after schema v1 validation. */
        public SignerEvidence(String apk, String certificateSha256) {
            if (!isAsset(apk) || !isHash(certificateSha256)) throw new IllegalArgumentException("invalid signer evidence");
            this.apk = apk;
            this.certificateSha256 = certificateSha256;
        }
    }

    public static final class Candidate {
        private final Version version;
        private final String apk;
        private final String checksum;
        private final String signer;
        private Candidate(Version version, String apk, String checksum, String signer) {
            this.version = version; this.apk = apk; this.checksum = checksum; this.signer = signer;
        }
        public String apkAsset() { return apk; }
        public String checksumAsset() { return checksum; }
        public String signerAsset() { return signer; }
        @Override public String toString() { return "ReleaseCandidate(version=" + version + ", assets=REDACTED)"; }
    }

    /** Verified immutable bytes to pass to an injected app-private staging writer. */
    public static final class StagingPlan {
        private final Version version;
        private final String apk;
        private final String sha256;
        private final byte[] bytes;
        private StagingPlan(Version version, String apk, String sha256, byte[] bytes) {
            this.version = version; this.apk = apk; this.sha256 = sha256; this.bytes = bytes;
        }
        public String apkAsset() { return apk; }
        public String sha256() { return sha256; }
        public byte[] copyApkBytesForStaging() { return bytes.clone(); }
        @Override public String toString() { return "VerifiedApkStagingPlan(version=" + version + ", asset=REDACTED)"; }
    }

    private static final class Version implements Comparable<Version> {
        private final int major, minor, patch;
        private Version(int major, int minor, int patch) { this.major = major; this.minor = minor; this.patch = patch; }
        static Version parseTag(String tag) { if (tag == null || !tag.matches("v[0-9]+\\.[0-9]+\\.[0-9]+")) throw new IllegalArgumentException("invalid tag"); return parse(tag.substring(1)); }
        static Version parseInstalled(String version) { if (version == null || !version.matches("[0-9]+\\.[0-9]+\\.[0-9]+")) throw new IllegalArgumentException("invalid version"); return parse(version); }
        private static Version parse(String value) { String[] parts = value.split("\\."); try { return new Version(Integer.parseInt(parts[0]), Integer.parseInt(parts[1]), Integer.parseInt(parts[2])); } catch (NumberFormatException exception) { throw new IllegalArgumentException("invalid version"); } }
        @Override public int compareTo(Version other) { int first = Integer.compare(major, other.major); if (first != 0) return first; int second = Integer.compare(minor, other.minor); return second != 0 ? second : Integer.compare(patch, other.patch); }
        @Override public boolean equals(Object value) { if (!(value instanceof Version)) return false; Version other = (Version) value; return major == other.major && minor == other.minor && patch == other.patch; }
        @Override public String toString() { return major + "." + minor + "." + patch; }
    }

    private static String requireTag(String tag) { if (tag == null || tag.length() > 64) throw new IllegalArgumentException("invalid tag"); return tag; }
    private static boolean isAsset(String asset) { return asset != null && asset.length() <= 128 && asset.matches("[A-Za-z0-9][A-Za-z0-9._-]*"); }
    public static final class UpdateException extends Exception { public UpdateException() { super(); } }
}
