# M0 Android verification evidence

The M0 Android boundary is verified without inferring ABI support from Gradle
configuration alone.

- `NativeAbiPackagingTest` requires exactly `arm64-v8a` and `x86_64`, validates
  ELF64 little-endian machine identifiers, and locks each bridge binary SHA-256.
- `SmokeInstrumentation` checks the target and launcher package are `ai.choosh`,
  launches the resolved component synchronously, observes active and teardown
  lifecycle states, and verifies the visible and accessibility labels.
- The instrumentation result bundle reports only stable package, activity,
  lifecycle, and label evidence; it contains no device-specific timing or paths.

Run the headless checks with `./gradlew :app:testDebugUnitTest`. Build an APK with
`./gradlew :app:assembleDebug`, then inspect it to ensure both
`lib/arm64-v8a/libchoosh_android_bridge.so` and
`lib/x86_64/libchoosh_android_bridge.so` are packaged. Run the instrumentation on
each supported emulator/device ABI with `./gradlew :app:connectedDebugAndroidTest`.
Device instrumentation remains device evidence and is not replaced by the headless
ABI test.
