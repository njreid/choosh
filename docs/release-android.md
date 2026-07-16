# Android release and Obtainium distribution

The release workflow publishes exactly one signed universal APK named
`choosh-VERSION.apk`, plus its SHA-256 list, CycloneDX SBOM, dependency notices,
and GitHub artifact provenance. It never builds split APKs for the release lane.

## Android build contract

The future `android/app` composition root must expose Gradle as module `:app` and:

- use application ID `ai.choosh`;
- read `CHOOSH_VERSION_NAME` and `CHOOSH_VERSION_CODE` for release builds;
- read `CHOOSH_KEYSTORE_FILE`, `CHOOSH_KEYSTORE_PASSWORD`, `CHOOSH_KEY_ALIAS`, and
  `CHOOSH_KEY_PASSWORD`, failing release validation when any value is absent;
- produce one universal APK below `android/app/build/outputs/apk/release/`;
- expose `cyclonedxBom`, producing `android/app/build/reports/bom.json`;
- expose `generateReleaseLicenseReport`, producing
  `android/app/build/reports/licenses/NOTICE.txt`.

The GitHub repository needs four Actions secrets: `CHOOSH_KEYSTORE_BASE64`,
`CHOOSH_KEYSTORE_PASSWORD`, `CHOOSH_KEY_ALIAS`, and `CHOOSH_KEY_PASSWORD`. The
keystore and passwords must be backed up separately. Losing or changing the key
breaks the Android update path. Secrets must never appear in workflow artifacts,
logs, example values, or repository files.

Tags must be exact `vMAJOR.MINOR.PATCH`. The workflow derives Android
`versionCode` as `MAJOR * 1,000,000 + MINOR * 1,000 + PATCH`; each component is
bounded, and every published version must increase numerically. A manual run can
verify artifacts but does not publish a GitHub Release.

## Obtainium

Add the GitHub repository URL to Obtainium. It will select the single APK attached
to the newest GitHub Release. Updates install only when the APK uses the same
signing key as the installed build and its `versionCode` is larger. Do not mix
debug-signed and release-signed installations.

Before treating a release as complete, verify the GitHub Release contains exactly:

- `choosh-VERSION.apk`;
- `choosh-VERSION.sha256`;
- `choosh-VERSION.cdx.json`;
- `choosh-VERSION-NOTICE.txt`;
- the GitHub provenance attestation associated with the APK.
