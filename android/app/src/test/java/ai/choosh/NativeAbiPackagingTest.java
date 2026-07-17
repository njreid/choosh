package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Set;
import java.util.stream.Collectors;
import org.junit.Test;

public final class NativeAbiPackagingTest {
    private static final Path JNI_LIBS = Path.of("src", "main", "jniLibs");
    private static final String LIBRARY = "libchoosh_android_bridge.so";

    @Test public void bridgeHasExactSupportedAbiSetAndElfMachines() throws IOException {
        Set<String> abiDirectories;
        try (var entries = Files.list(JNI_LIBS)) {
            abiDirectories = entries.filter(Files::isDirectory)
                .map(path -> path.getFileName().toString())
                .collect(Collectors.toSet());
        }
        assertEquals(Set.of("arm64-v8a", "x86_64"), abiDirectories);
        assertElf("arm64-v8a", 183, "ac5833e48e220589e9118e672293e9593fa22a3d5e9d71b8c26780493cf8f872");
        assertElf("x86_64", 62, "9e69a4110bd249fdcb7dec2602205beaf10ca878abc59ebcacb489b2332a4f8f");
    }

    private static void assertElf(String abi, int expectedMachine, String expectedSha256)
        throws IOException {
        byte[] bytes = Files.readAllBytes(JNI_LIBS.resolve(abi).resolve(LIBRARY));
        assertTrue("native bridge must contain a real ELF payload", bytes.length > 64);
        assertArrayEquals(new byte[] {0x7f, 'E', 'L', 'F'}, java.util.Arrays.copyOf(bytes, 4));
        assertEquals("ELF64 required", 2, Byte.toUnsignedInt(bytes[4]));
        assertEquals("little-endian ELF required", 1, Byte.toUnsignedInt(bytes[5]));
        int machine = Byte.toUnsignedInt(bytes[18]) | (Byte.toUnsignedInt(bytes[19]) << 8);
        assertEquals(expectedMachine, machine);
        assertEquals(expectedSha256, sha256(bytes));
    }

    private static String sha256(byte[] bytes) {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256").digest(bytes);
            StringBuilder text = new StringBuilder(digest.length * 2);
            for (byte value : digest) { text.append(String.format("%02x", Byte.toUnsignedInt(value))); }
            return text.toString();
        } catch (NoSuchAlgorithmException impossible) {
            throw new AssertionError("SHA-256 must exist in the Java runtime", impossible);
        }
    }
}
