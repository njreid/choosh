package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
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
        assertElf("arm64-v8a", 183);
        assertElf("x86_64", 62);
    }

    private static void assertElf(String abi, int expectedMachine) throws IOException {
        byte[] bytes = Files.readAllBytes(JNI_LIBS.resolve(abi).resolve(LIBRARY));
        assertTrue("native bridge must contain a real ELF payload", bytes.length > 64);
        assertArrayEquals(new byte[] {0x7f, 'E', 'L', 'F'}, java.util.Arrays.copyOf(bytes, 4));
        assertEquals("ELF64 required", 2, Byte.toUnsignedInt(bytes[4]));
        assertEquals("little-endian ELF required", 1, Byte.toUnsignedInt(bytes[5]));
        int machine = Byte.toUnsignedInt(bytes[18]) | (Byte.toUnsignedInt(bytes[19]) << 8);
        assertEquals(expectedMachine, machine);
    }
}
