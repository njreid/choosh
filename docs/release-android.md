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

Pull-request CI does not query GitHub or publish a release. Instead,
`scripts/test-release-discovery.sh` exercises a bounded local GitHub-release
metadata fixture. The gate selects the highest stable `vMAJOR.MINOR.PATCH`,
requires one version-matched universal APK and checksum, verifies the APK digest,
requires signer evidence to name that same APK, and checks signing-identity
continuity with the preceding stable fixture. Negative fixtures prove that
checksum substitution, signer/APK misassociation, and signing-identity changes
fail closed. The signer JSON
records selection evidence only; cryptographic APK signature verification remains
the release workflow's `apksigner verify` responsibility.

Before treating a release as complete, verify the GitHub Release contains exactly:

- `choosh-VERSION.apk`;
- `choosh-VERSION.sha256` (the APK's digest only, in `sha256sum` format — not a
  combined hash of every release asset);
- `choosh-VERSION.apk.signer.json` (the signer evidence named above);
- `choosh-VERSION.cdx.json`;
- `choosh-VERSION-NOTICE.txt`;
- the GitHub provenance attestation associated with the APK.

## LGPL component evidence

Sora Editor is an accepted LGPL-2.1-or-later dependency. Acceptance does not by
itself establish that a particular APK distribution satisfies every applicable
obligation. Before a release containing Sora is promoted, retain the evidence
listed in [the Sora packaging record](licenses/sora-packaging.md): the exact
resolved artifact/source identity, the complete notice and licence text, Choosh
modification status, and the source/replacement information made available with
the release. The release gate must fail closed if that record is incomplete.

This is an engineering evidence requirement, not legal advice. The release owner
remains responsible for obtaining legal review where the selected packaging or
distribution model requires it.
