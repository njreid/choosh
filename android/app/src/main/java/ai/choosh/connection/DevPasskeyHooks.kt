package ai.choosh.connection

import ai.choosh.BuildConfig
import ai.choosh.nativeDevPasskeyRegister
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Escape hatch for devices with no real `WebAuthn` credential provider
 * (e.g. this project's own Genymotion test instances, which have no
 * Google Play Services — `androidx.credentials.CredentialManager.createCredential`
 * fails there with `CreateCredentialNoCreateOptionException: No create
 * options available.` before any ceremony can even start).
 *
 * **Gating is `BuildConfig.DEBUG`, not a Gradle source-set split**: an
 * earlier version of this file tried to put the real implementation only
 * in `src/debug/java/ai/choosh/connection/DevPasskeyHooks.kt`, expecting
 * AGP's `debug`-overrides-`main` precedence (real, and correctly relied on
 * elsewhere in this project, e.g. `jniLibs/`) to apply — it doesn't, for
 * compiled Kotlin/Java sources: `main` and `debug` are both fully compiled
 * into the `debug` variant, so an identically-named class in both is a
 * genuine duplicate-declaration error, not an override. `BuildConfig.DEBUG`
 * (`true` for `debug`, `false` for `release`, fixed per variant by AGP
 * itself) is the correct, standard mechanism for this.
 *
 * **This still cannot work in a release build**, for a stronger reason
 * than the `BuildConfig.DEBUG` check alone: the real cryptography lives in
 * `rust/choosh-android-bridge/src/dev_passkey.rs`, compiled in only when
 * the `dev-passkey` Cargo feature is enabled — which only
 * `android/app/build.gradle.kts`'s `buildRustAndroidDevPasskey` task ever
 * passes, writing its output to `src/debug/jniLibs/` (jniLibs *does*
 * correctly get AGP's debug-overrides-main treatment, unlike Kotlin
 * sources — verified: `nm -D` on the plain `buildRustAndroid` output shows
 * no `nativeDevPasskeyRegister` symbol at all). A release `.so` genuinely
 * has no such symbol to call, so even a `BuildConfig.DEBUG` bypass (e.g. a
 * decompiled/patched APK) would only get `UnsatisfiedLinkError`, not a
 * working ceremony — `relayd`'s own `WebAuthn` verification is completely
 * real and untouched either way; this only replaces the *client-side*
 * ceremony normally fulfilled by the OS's platform authenticator.
 */
object DevPasskeyHooks {
    val available: Boolean = BuildConfig.DEBUG

    suspend fun register(creationOptionsJson: String): String {
        check(BuildConfig.DEBUG) { "dev passkey ceremony is not available in this build" }
        return withContext(Dispatchers.IO) {
            nativeDevPasskeyRegister(BuildConfig.CHOOSH_RELAYD_HTTP_URL, creationOptionsJson)
        }
    }
}
