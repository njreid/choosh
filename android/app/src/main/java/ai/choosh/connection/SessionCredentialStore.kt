package ai.choosh.connection

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Storage for the one opaque bearer credential this app ever holds
 * (auth-and-enrollment.md's phone session credential — never a password,
 * never the passkey itself). "Every later app open reuses that stored
 * credential silently" (auth-and-enrollment.md) is exactly what this makes
 * possible: [load] on cold start, [save] once after a successful `WebAuthn`
 * ceremony, [clear] on an explicit sign-out or a revoked-credential
 * rejection from `relayd`.
 *
 * An interface (not a concrete class), mirroring
 * [ai.choosh.webservice.WebServiceGatewayController]'s "abstraction + Real
 * impl" precedent — [RealSessionCredentialStore] is the real,
 * `EncryptedSharedPreferences`-backed implementation; a plain in-memory test
 * double stands in for it in [ConnectionViewModel]'s own JVM unit tests
 * (`RealSessionCredentialStore`'s Android Keystore dependency has no JVM
 * unit test equivalent, the same reason [WebServiceGatewayController] itself
 * needed one).
 */
interface SessionCredentialStore {
    fun load(): String?
    fun save(sessionCredential: String)
    fun clear()
}

/** The real, `EncryptedSharedPreferences`-backed [SessionCredentialStore]. */
class RealSessionCredentialStore(context: Context) : SessionCredentialStore {
    private val prefs: SharedPreferences = run {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context,
            PREFS_FILE_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun load(): String? = prefs.getString(KEY_SESSION_CREDENTIAL, null)

    override fun save(sessionCredential: String) {
        prefs.edit().putString(KEY_SESSION_CREDENTIAL, sessionCredential).apply()
    }

    override fun clear() {
        prefs.edit().remove(KEY_SESSION_CREDENTIAL).apply()
    }

    companion object {
        private const val PREFS_FILE_NAME = "choosh_session_credential"
        private const val KEY_SESSION_CREDENTIAL = "session_credential"
    }
}
