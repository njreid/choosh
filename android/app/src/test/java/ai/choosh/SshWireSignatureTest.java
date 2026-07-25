package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import org.junit.Test;

/** Headless wire vectors for the Android Keystore-to-Russh Ed25519 adapter. */
public final class SshWireSignatureTest {
    @Test public void appends_exact_ssh_ed25519_signature_envelope() {
        byte[] raw = new byte[64];
        raw[0] = 9;
        raw[63] = 8;
        byte[] encoded = SshWireSignature.appendEd25519(new byte[] {1, 2}, raw);
        ByteBuffer bytes = ByteBuffer.wrap(encoded);
        assertEquals(1, bytes.get());
        assertEquals(2, bytes.get());
        assertEquals(83, bytes.getInt());
        assertEquals(11, bytes.getInt());
        byte[] algorithm = new byte[11];
        bytes.get(algorithm);
        assertArrayEquals("ssh-ed25519".getBytes(StandardCharsets.US_ASCII), algorithm);
        assertEquals(64, bytes.getInt());
        byte[] returned = new byte[64];
        bytes.get(returned);
        assertArrayEquals(raw, returned);
        assertEquals(0, bytes.remaining());
    }

    @Test public void rejects_values_that_cannot_fit_the_native_callback_bound() {
        assertThrows(IllegalArgumentException.class,
            () -> SshWireSignature.appendEd25519(new byte[0], new byte[64]));
        assertThrows(IllegalArgumentException.class,
            () -> SshWireSignature.appendEd25519(
                new byte[SshWireSignature.maximumEd25519PayloadBytes() + 1], new byte[64]));
        assertThrows(IllegalArgumentException.class,
            () -> SshWireSignature.appendEd25519(new byte[] {1}, new byte[63]));
    }
}
