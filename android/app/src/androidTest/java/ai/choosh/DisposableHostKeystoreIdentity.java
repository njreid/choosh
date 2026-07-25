package ai.choosh;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.Signature;

/** Test-only non-exportable Ed25519 identity for the disposable-host instrumentation gate. */
public final class DisposableHostKeystoreIdentity
    implements DisposableHostInstrumentationComposition.Identity,
        BoundedAndroidNativeRuntime.PublicKeySource,
        BoundedAndroidNativeRuntime.LeaseSignerSource {
    private static final String ALIAS = "choosh.m0.disposable-host.ed25519";
    private static final byte[] ALGORITHM = "ssh-ed25519".getBytes(StandardCharsets.US_ASCII);
    private final byte[] openSsh;
    private final String openSshLine;
    private final SshKeyImportCoordinator.OpaqueCredentialRef credential;
    private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

    private DisposableHostKeystoreIdentity(byte[] openSsh, String openSshLine, String fingerprint) {
        this.openSsh = openSsh;
        this.openSshLine = openSshLine;
        credential = new SshKeyImportCoordinator.OpaqueCredentialRef(ALIAS);
        publicKey = new SshKeyImportCoordinator.PublicKeyMetadata(
            SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519, fingerprint
        );
    }

    /** Creates or reuses the test alias; private-key bytes never leave Android Keystore. */
    public static DisposableHostKeystoreIdentity open() throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (!store.containsAlias(ALIAS)) {
            KeyPairGenerator generator = KeyPairGenerator.getInstance(
                "Ed25519", "AndroidKeyStore"
            );
            generator.initialize(new KeyGenParameterSpec.Builder(
                ALIAS, KeyProperties.PURPOSE_SIGN | KeyProperties.PURPOSE_VERIFY
            ).setDigests(KeyProperties.DIGEST_NONE).build());
            generator.generateKeyPair();
        }
        PublicKey key = store.getCertificate(ALIAS).getPublicKey();
        byte[] encoded = key.getEncoded();
        if (encoded.length != 44) throw new IllegalStateException("unexpected Ed25519 public key");
        byte[] raw = new byte[32];
        System.arraycopy(encoded, encoded.length - raw.length, raw, 0, raw.length);
        byte[] wire = ByteBuffer.allocate(4 + ALGORITHM.length + 4 + raw.length)
            .putInt(ALGORITHM.length).put(ALGORITHM).putInt(raw.length).put(raw).array();
        String keyText = "ssh-ed25519 " + Base64.encodeToString(wire, Base64.NO_WRAP);
        String fingerprint = "SHA256:" + Base64.encodeToString(
            MessageDigest.getInstance("SHA-256").digest(wire), Base64.NO_WRAP | Base64.NO_PADDING
        );
        return new DisposableHostKeystoreIdentity(keyText.getBytes(StandardCharsets.US_ASCII), keyText, fingerprint);
    }

    /** Non-secret line suitable only for the fixture's generated authorized_keys file. */
    public String authorizedKeyLine() { return openSshLine; }
    @Override public SshKeyImportCoordinator.OpaqueCredentialRef credential() { return credential; }
    @Override public SshKeyImportCoordinator.PublicKeyMetadata publicKey() { return publicKey; }
    @Override public byte[] publicKey(NativeAuthenticatedSshConnector.NativeConnectionInput ignored) {
        return openSsh.clone();
    }
    @Override public BoundedAndroidNativeRuntime.LeaseSigner bind(
        NativeAuthenticatedSshConnector.NativeConnectionInput ignored
    ) {
        return payload -> {
            try {
                KeyStore store = KeyStore.getInstance("AndroidKeyStore");
                store.load(null);
                PrivateKey key = (PrivateKey) store.getKey(ALIAS, null);
                if (key == null) throw new RustNativeConnectorJni.NativePlanException();
                Signature signature = Signature.getInstance("Ed25519");
                signature.initSign(key);
                signature.update(payload);
                return SshWireSignature.appendEd25519(payload, signature.sign());
            } catch (RustNativeConnectorJni.NativePlanException exception) {
                throw exception;
            } catch (Exception exception) {
                throw new RustNativeConnectorJni.NativePlanException();
            }
        };
    }
}
