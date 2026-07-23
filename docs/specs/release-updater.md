# Android release updater

Status: Draft

## Scope

This protocol selects and verifies a published Android APK before an outer
Android installer stages it. It does not update `chooshd`, open SSH, execute a
host command, choose a host path, or grant package-install authority.

## Release index

The download edge supplies at most 100 releases. Each release has a tag, draft
and prerelease flags, and at most 32 canonical asset names. A candidate is a
unique, non-draft, non-prerelease tag of exact form `vMAJOR.MINOR.PATCH`, and
it MUST be strictly newer than the installed version.

For selected version `X`, its release MUST contain exactly once:

- `choosh-X.apk`;
- `choosh-X.sha256`; and
- `choosh-X.apk.signer.json`.

It MUST contain no other stable-version APK. Asset names are identifiers, not
paths or URLs, and match `^[A-Za-z0-9][A-Za-z0-9._-]*$`.

## Evidence and staging

The APK is limited to 256 MiB, the checksum file to 256 bytes, and signer
evidence to 4 KiB. The checksum file has exactly one ASCII line:

```text
lowercase-64-hex-sha256␠␠choosh-X.apk
```

Signer evidence schema v1 has exactly `schema_version`, `apk`, and
`certificate_sha256`; its APK name matches the selected APK and its lowercase
certificate digest matches the installed trusted signing identity. An injected
APK certificate inspector MUST independently report the same digest for the
downloaded bytes. The updater computes SHA-256 itself and compares it to the
checksum before returning an immutable staging plan.

The staging plan is data only. An Android outer adapter may write it only to an
app-private location and request package installation only through the platform
user-mediated installer. A verification failure returns a content-free update
failure and produces no staging plan.

## Verification

Headless tests MUST prove newest-stable selection, rejection of duplicate APKs,
non-monotonic releases, checksum mismatch, signer-evidence mismatch, and APK
certificate mismatch. A real release lane additionally verifies the generated
signer-evidence asset and two Obtainium upgrades.
