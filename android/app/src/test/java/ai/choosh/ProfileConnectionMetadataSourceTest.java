package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

/** Deterministic profile metadata proof; no Android service, socket, or credential material. */
public final class ProfileConnectionMetadataSourceTest {
    private static final AuthenticatedSshOperationCoordinator.ProfileId PROFILE =
        new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile");
    private static final String FINGERPRINT =
        "SHA256:0123456789012345678901234567890123456789012";

    @Test public void mapsOneMatchingDurableProfileToExactConnectionRequest() throws Exception {
        ProfileConnectionMetadataSource.ProfileConnectionMetadata metadata = metadata(PROFILE);
        ProfileConnectionMetadataSource source = new ProfileConnectionMetadataSource(profileId -> metadata);

        AuthenticatedSshOperationCoordinator.ConnectionRequest request = source.connectionFor(PROFILE);

        assertSame(PROFILE, request.profileId());
        assertEquals("ssh-fixture.example", request.endpoint().hostForNativeConnector());
        assertEquals(22, request.endpoint().portForNativeConnector());
        assertEquals("fixture_user", request.username().valueForNativeConnector());
        assertEquals(ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519, request.knownHost().algorithm());
        assertEquals(FINGERPRINT, request.knownHost().sha256FingerprintForVerifier());
        assertEquals("ConnectionRequest(profile=ProfileId(REDACTED), endpoint=REDACTED, username=REDACTED, knownHost=ED25519, credential=REDACTED, publicKey=ED25519)", request.toString());
    }

    @Test public void missingOrSubstitutedProfileFailsBeforeConnectorCanOpen() {
        ProfileConnectionMetadataSource missing = new ProfileConnectionMetadataSource(profileId -> null);
        ProfileConnectionMetadataSource substituted = new ProfileConnectionMetadataSource(
            profileId -> metadata(new AuthenticatedSshOperationCoordinator.ProfileId("other_profile"))
        );

        assertThrows(AuthenticatedSshOperationCoordinator.ProfileUnavailableException.class,
            () -> missing.connectionFor(PROFILE));
        assertThrows(AuthenticatedSshOperationCoordinator.ProfileUnavailableException.class,
            () -> substituted.connectionFor(PROFILE));
    }

    @Test public void endpointKnownHostAndUsernameRejectAmbiguousOrUnsafeInputs() {
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.SshEndpoint("ssh://fixture.example", 22));
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.SshEndpoint("fixture.example", 0));
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.SshEndpoint("fixture..example", 22));
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.SshUsername("fixture user"));
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.SshUsername("fixture\nuser"));
        assertThrows(IllegalArgumentException.class,
            () -> new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519, "SHA256:with-padding="
            ));
    }

    private static ProfileConnectionMetadataSource.ProfileConnectionMetadata metadata(
        AuthenticatedSshOperationCoordinator.ProfileId profileId
    ) {
        return new ProfileConnectionMetadataSource.ProfileConnectionMetadata(
            profileId,
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture_user"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519, FINGERPRINT
            ),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_fixture_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                FINGERPRINT
            )
        );
    }
}
