# Android and Kotlin toolchain

## Policy

Choosh MUST use the latest mutually compatible **stable** Android and Kotlin toolchain available when a milestone begins. Versions MUST be pinned in the Gradle version catalog and wrapper; dynamic versions such as `+` and unversioned snapshots are forbidden.

Preview SDKs and pre-release libraries MAY be exercised in a non-blocking CI lane, but MUST NOT be required to build or release the production application. The minimum supported Android version is a product compatibility decision and MUST NOT rise merely because a newer compile or target SDK exists.

## Baseline

The baseline resolved on 2026-07-14 is:

| Component | Production baseline | Notes |
| --- | --- | --- |
| Android platform | `compileSdk = 36`, `targetSdk = 36` | Android 16 is the latest stable platform; API 37 remains an Android 17 preview. |
| Android 17 validation | API 37 | Non-blocking preview lane until Android 17 is stable. |
| Android Gradle Plugin | 9.2.1 | Current stable patch release. |
| Gradle wrapper | 9.4.1 | Required by AGP 9.2. |
| Kotlin | 2.4.10 | Current stable Kotlin bug-fix release. |
| Java toolchain | JDK 17 | AGP 9.2 minimum and default. |
| SDK Build Tools | 36.0.0 | AGP 9.2 default. |
| NDK | 28.2.13676358 | AGP 9.2 default; required for the Rust Android bridge. |
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

## Sources

- [Android 17 SDK setup](https://developer.android.com/about/versions/17/setup-sdk)
- [Android Gradle Plugin 9.2 release notes](https://developer.android.com/build/releases/agp-9-2-0-release-notes)
- [Kotlin releases](https://kotlinlang.org/docs/releases.html)
- [Compose BOM mapping](https://developer.android.com/develop/ui/compose/bom/bom-mapping)

