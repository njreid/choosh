package ai.choosh.connection

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Keystore-backed storage for the one opaque bearer credential this app
 * ever holds (auth-and-enrollment.md's phone session credential — never a
 * password, never the passkey itself). "Every later app open reuses that
 * stored credential silently" (auth-and-enrollment.md) is exactly what this
 * makes possible: [load] on cold start, [save] once after a successful
 * `WebAuthn` ceremony, [clear] on an explicit sign-out or a revoked-credential
 * rejection from `relayd`.
 */
class SessionCredentialStore(context: Context) {
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

    fun load(): String? = prefs.getString(KEY_SESSION_CREDENTIAL, null)

    fun save(sessionCredential: String) {
        prefs.edit().putString(KEY_SESSION_CREDENTIAL, sessionCredential).apply()
    }

    fun clear() {
        prefs.edit().remove(KEY_SESSION_CREDENTIAL).apply()
    }

    companion object {
        private const val PREFS_FILE_NAME = "choosh_session_credential"
        private const val KEY_SESSION_CREDENTIAL = "session_credential"
    }
}
