# ADR 0006: Android minimum SDK 26

## Status

Accepted for the initial Android skeleton.

## Decision

Choosh uses `minSdk = 26` (Android 8.0) while compiling and targeting API 37.
Newer platform APIs remain guarded by AndroidX or runtime SDK checks.

API 26 provides notification channels, modern Android Keystore primitives, and
the WebView and process-lifecycle baseline needed by Choosh without coupling the
minimum supported version to the production target SDK. The SSH, Rust bridge,
Sora editor, and native renderer integrations MUST retain API 26 compatibility
as they are introduced; a dependency that requires raising the floor needs a
new compatibility report and an ADR amendment.

## Verification

The debug manifest is inspected for `minSdkVersion=26` and `targetSdkVersion=37`.
JVM and instrumentation suites compile against API 37. Device acceptance needs
both the API 37 primary lane and an API 26 compatibility lane before release.
