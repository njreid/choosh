# Android and Kotlin toolchain

## Policy

Choosh MUST use the latest mutually compatible **stable** Android and Kotlin toolchain available when a milestone begins. Versions MUST be pinned in the Gradle version catalog and wrapper; dynamic versions such as `+` and unversioned snapshots are forbidden.

Preview SDKs and pre-release libraries MAY be exercised in a non-blocking CI lane, but MUST NOT be required to build or release the production application. The minimum supported Android version is a product compatibility decision and MUST NOT rise merely because a newer compile or target SDK exists.

## Baseline

The release baseline re-resolved on 2026-07-19 is:

| Component | Production baseline | Notes |
| --- | --- | --- |
| Android platform | `compileSdk = 36`, `targetSdk = 36` | Android 16 is the latest stable platform available to the release SDK channel. |
| Next-platform validation | No post-API-36 platform SDK published as of 2026-07-19 | Quarterly previews remain outside the production release baseline. |
| Android Gradle Plugin | 9.3.0 | Current stable minor release. |
| Gradle wrapper | 9.6.1 | Current stable wrapper; checksum pinned. |
| Kotlin | 2.4.10 | Current stable Kotlin bug-fix release. |
| Java toolchain | JDK 17 | AGP 9.3 minimum and default. |
| SDK Build Tools | 36.0.0 | AGP 9.3 default. |
| NDK | 28.2.13676358 | AGP 9.3 default; required for the Rust Android bridge. |
| Compose BOM | 2026.06.01 | Latest stable BOM in the official mapping. |

`minSdk` remains an M0 decision. It MUST be chosen from the actual requirements of Sora Editor, SSH, notifications, WebView security, storage, and the supported-device policy. Newer APIs MUST be guarded with AndroidX compatibility layers or explicit runtime SDK checks where the chosen `minSdk` requires them.

## Compatibility rule

Sora Editor and the Kotlin/Rust bridge are release-critical dependencies. M0 MUST prove that they compile, test, and package with the baseline. If the newest stable components are not mutually compatible, Choosh MAY pin the newest working stable combination only when:

1. the incompatibility is reproduced in CI;
2. the exception and security impact are recorded in an ADR;
3. an upgrade issue names the blocked component and removal condition; and
4. the exception does not require a preview dependency in production.

## Dependency management

- All plugin and library versions MUST be centralized in `gradle/libs.versions.toml`.
- Compose libraries covered by the BOM MUST omit individual versions.
- Dependency verification and locking MUST be enabled for release builds.
- Stable AndroidX releases are preferred; alpha, beta, RC, and snapshot artifacts require an ADR and an expiry condition.
- Automated dependency update pull requests SHOULD run weekly and MUST pass unit, instrumentation, lint, Sora editing, Rust bridge, and packaging tests before merge.
- New code MUST NOT introduce deprecated Android APIs unless no supported replacement exists and the exception is documented.
- Each milestone start and release candidate MUST re-resolve this table against official release notes. Upgrades are reviewed changes, never dynamic resolution during a build.

## Preview compatibility lane

The `android-next-platform-preview` CI job queries the preview SDK channel for a
nonnumeric `platforms;android-<codename>` package. If Google has not published a
post-API-36 next-platform SDK, the job emits the stable
`preview_sdk_status=no_next_platform_preview_available` evidence and succeeds
without pretending that a QPR runtime image is a new compile SDK. When a preview
package exists, the job installs that exact discovered package, changes only its
ephemeral checkout to `compileSdkPreview`, and runs lint, unit tests, and debug
assembly. It has `continue-on-error: true`, receives no publishing credentials,
uploads no artifacts, and is not a dependency of production or release jobs.

## Sources

- [Android 16 SDK setup](https://developer.android.com/about/versions/16/setup-sdk)
- [Android Gradle Plugin 9.3 release notes](https://developer.android.com/build/releases/agp-9-3-0-release-notes)
- [Gradle 9.6.1 release notes](https://docs.gradle.org/9.6.1/release-notes.html)
- [Kotlin releases](https://kotlinlang.org/docs/releases.html)
- [Compose BOM mapping](https://developer.android.com/develop/ui/compose/bom/bom-mapping)
