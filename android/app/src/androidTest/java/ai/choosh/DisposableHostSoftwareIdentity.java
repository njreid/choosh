package ai.choosh;

import android.content.Context;
import android.util.Base64;
import java.math.BigInteger;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.Signature;
import java.security.interfaces.RSAPublicKey;
import java.security.spec.PKCS8EncodedKeySpec;
import java.security.spec.X509EncodedKeySpec;

/** Explicit test-only software signer used only when emulator Keystore is unavailable. */
public final class DisposableHostSoftwareIdentity
    implements DisposableHostInstrumentationComposition.Identity,
        BoundedAndroidNativeRuntime.PublicKeySource,
        BoundedAndroidNativeRuntime.LeaseSignerSource {
    private static final String PREFS = "choosh_disposable_software_fixture";
    private static final String PRIVATE = "private_pkcs8";
    private static final String PUBLIC = "public_x509";
    private static final byte[] ALGORITHM = "ssh-rsa".getBytes(StandardCharsets.US_ASCII);
    private final KeyPair pair;
    private final byte[] wire;
    private final String line;
    private final SshKeyImportCoordinator.OpaqueCredentialRef credential =
        new SshKeyImportCoordinator.OpaqueCredentialRef("software_fixture_rsa");
    private final SshKeyImportCoordinator.PublicKeyMetadata publicKey;

    private DisposableHostSoftwareIdentity(KeyPair pair) throws Exception {
        this.pair = pair;
        RSAPublicKey rsa = (RSAPublicKey) pair.getPublic();
        byte[] exponent = mpint(rsa.getPublicExponent());
        byte[] modulus = mpint(rsa.getModulus());
        wire = ByteBuffer.allocate(4 + ALGORITHM.length + 4 + exponent.length + 4 + modulus.length)
            .putInt(ALGORITHM.length).put(ALGORITHM).putInt(exponent.length).put(exponent)
            .putInt(modulus.length).put(modulus).array();
        line = "ssh-rsa " + Base64.encodeToString(wire, Base64.NO_WRAP);
        String fingerprint = "SHA256:" + Base64.encodeToString(
            java.security.MessageDigest.getInstance("SHA-256").digest(wire), Base64.NO_WRAP | Base64.NO_PADDING
        );
        publicKey = new SshKeyImportCoordinator.PublicKeyMetadata(
            SshKeyImportCoordinator.SshPublicKeyAlgorithm.RSA, fingerprint
        );
    }

    public static DisposableHostSoftwareIdentity open(Context context) throws Exception {
        android.content.SharedPreferences prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        KeyFactory factory = KeyFactory.getInstance("RSA");
        String privateText = prefs.getString(PRIVATE, null);
        String publicText = prefs.getString(PUBLIC, null);
        KeyPair pair;
        if (privateText == null || publicText == null) {
            KeyPairGenerator generator = KeyPairGenerator.getInstance("RSA");
            generator.initialize(2048);
            pair = generator.generateKeyPair();
            prefs.edit().putString(PRIVATE, Base64.encodeToString(pair.getPrivate().getEncoded(), Base64.NO_WRAP))
                .putString(PUBLIC, Base64.encodeToString(pair.getPublic().getEncoded(), Base64.NO_WRAP)).commit();
        } else {
            pair = new KeyPair(
                factory.generatePublic(new X509EncodedKeySpec(Base64.decode(publicText, Base64.DEFAULT))),
                factory.generatePrivate(new PKCS8EncodedKeySpec(Base64.decode(privateText, Base64.DEFAULT)))
            );
        }
        return new DisposableHostSoftwareIdentity(pair);
    }

    public String authorizedKeyLine() { return line; }
    @Override public SshKeyImportCoordinator.OpaqueCredentialRef credential() { return credential; }
    @Override public SshKeyImportCoordinator.PublicKeyMetadata publicKey() { return publicKey; }
    @Override public byte[] publicKey(NativeAuthenticatedSshConnector.NativeConnectionInput ignored) { return line.getBytes(StandardCharsets.US_ASCII); }
    @Override public BoundedAndroidNativeRuntime.LeaseSigner bind(NativeAuthenticatedSshConnector.NativeConnectionInput ignored) {
        return payload -> {
            try {
                Signature signature = Signature.getInstance("SHA256withRSA");
                signature.initSign((PrivateKey) pair.getPrivate());
                signature.update(payload);
                return SshWireSignature.appendRsaSha256(payload, signature.sign());
            } catch (Exception exception) {
                throw new RustNativeConnectorJni.NativePlanException();
            }
        };
    }

    public void wipe(Context context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().clear().commit();
    }

    private static byte[] mpint(BigInteger value) {
        byte[] bytes = value.toByteArray();
        return bytes[0] == 0 ? java.util.Arrays.copyOfRange(bytes, 1, bytes.length) : bytes;
    }
}
