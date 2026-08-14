# Android and Kotlin toolchain

Status: Draft

## Policy

Choosh MUST use the latest mutually compatible **stable** release of
Kotlin, the Android Gradle Plugin, the Compose BOM, and the target/compile
SDK available when a milestone begins — matching [AGENTS.md](../../AGENTS.md)'s
"Android and Kotlin dependencies are pinned stable releases; preview SDKs
run only in a separate compatibility lane." Versions MUST be pinned in the
Gradle version catalog (`gradle/libs.versions.toml`) and wrapper; dynamic
versions such as `+` and unversioned snapshots are forbidden. Specific
version numbers are deliberately not restated here — they belong in the
version catalog, where they can't rot into a stale doc.

Preview SDKs and pre-release libraries MAY be exercised in a non-blocking
CI lane, but MUST NOT be required to build or release the production
application. The minimum supported Android version (`minSdk`) is a product
compatibility decision, chosen from the actual requirements of Sora
Editor, the relay client transport, notifications, WebView security, and
storage — it MUST NOT rise merely because a newer compile or target SDK
exists.

## Compatibility rule

Sora Editor and the Kotlin/Rust bridge are release-critical dependencies.
Each milestone that touches them MUST prove they compile, test, and
package together at the current pinned baseline before that milestone's
exit criteria are considered met. If the newest stable components are not
mutually compatible, Choosh MAY pin the newest working stable combination
only when:

1. the incompatibility is reproduced in CI;
2. the exception and its scope are recorded in this file's baseline table
   (added when the first such exception occurs — none exist yet);
3. an upgrade issue names the blocked component and removal condition; and
4. the exception does not require a preview dependency in production.

## Dependency management

- All plugin and library versions MUST be centralized in
  `gradle/libs.versions.toml`.
- Compose libraries covered by the BOM MUST omit individual versions.
- Dependency verification and locking MUST be enabled for release builds.
- Stable AndroidX releases are preferred; alpha, beta, RC, and snapshot
  artifacts require a recorded exception (§ above) and an expiry
  condition.
- New code MUST NOT introduce deprecated Android APIs unless no supported
  replacement exists and the exception is documented.
- Each milestone start and release candidate MUST re-resolve the pinned
  versions against official release notes. Upgrades are reviewed changes,
  never dynamic resolution during a build.
