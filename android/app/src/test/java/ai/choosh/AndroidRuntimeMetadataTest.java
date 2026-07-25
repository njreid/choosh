package ai.choosh;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

/** Headless proof that runtime metadata excludes endpoint and credential selection. */
public final class AndroidRuntimeMetadataTest {
    @Test public void encodes_only_fixed_non_secret_identity_fields() {
        byte[] encoded = AndroidRuntimeMetadata.encode(
            new ProfileConnectionMetadataSource.SshUsername("fixture-user"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO12")
        );
        assertEquals(1, encoded[0]);
        assertEquals(12, encoded[1]);
        assertEquals('f', encoded[2]);
        assertEquals(50, encoded[14]);
        assertEquals('S', encoded[15]);
        assertEquals(124, encoded.length);
    }
}
