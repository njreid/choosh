# M0 Sora device instrumentation plan

Status: ready to run when an accelerated emulator or attached device is
available. This document records the device-only smoke gate; it does not replace
the headless adapter and Rust revision tests required by the M0 design.

## Local inventory and current blocker

Inventory captured on 2026-07-19:

- Android Emulator 36.6.11 and Platform Tools 37.0.0 are installed.
- AVD `medium_phone` uses the installed API 36 Google Play x86_64 image.
- API 37 is installed for compilation; the application targets API 37 and has
  `minSdk 26`.
- `emulator -accel-check` reports that KVM needs VMX or SVM and `/dev/kvm` is
  absent. ADB also cannot bind its local server socket in the restricted
  environment.

Consequently this checkout can compile the test APK, but it cannot produce valid
device evidence in the current environment. A software-rendered, unaccelerated
emulator is not the preferred acceptance route because startup reliability and
runtime make it unsuitable for a deterministic CI gate.

## Preferred headless route

Provide either Linux KVM access for `medium_phone` or an already booted API 26+
device reachable by ADB. Start the local AVD, when applicable, without a window:

```sh
/opt/android-sdk/emulator/emulator -avd medium_phone \
  -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect \
  -no-snapshot-save
```

In a second shell, wait for a completely booted device and run only the checked-in
instrumentation runner:

```sh
adb wait-for-device
test "$(adb shell getprop sys.boot_completed | tr -d '\r')" = "1"
cd android
ANDROID_HOME=/opt/android-sdk GRADLE_USER_HOME=/tmp/choosh-gradle \
  ./gradlew --no-daemon --no-configuration-cache --console=plain \
  :app:connectedDebugAndroidTest
```

For a shared ADB service or physical-device farm, omit emulator startup and select
exactly one device with `ANDROID_SERIAL=<serial>` before running the same Gradle
command. The selected device MUST report API 26 or newer and an ABI packaged by
the APK (`x86_64` or `arm64-v8a`):

```sh
test "$(adb shell getprop ro.build.version.sdk | tr -d '\r')" -ge 26
adb shell getprop ro.product.cpu.abi | tr -d '\r' | grep -Ex 'x86_64|arm64-v8a'
```

## Acceptance criteria

The run passes only when all of these conditions hold without a tap, screenshot
comparison, retry, or wall-clock assertion:

1. Gradle exits zero and reports the `SmokeInstrumentation` run as successful.
2. The runner constructs Sora `CodeEditor` 0.24.6, subscribes to the real
   `ContentChangeEvent`, sets `M0`, observes exactly one event with non-null
   change bounds and text, unsubscribes, and calls `release()`.
3. The result bundle contains `sora=0.24.6:setText-event-and-release`.
4. The runner translates synthetic real `ContentChangeEvent` insert/delete events
   using UTF-16 indices, preserves a non-BMP two-unit boundary, and rejects a
   full `ACTION_SET_NEW_TEXT` projection as an incremental edit. The result
   bundle contains
   `sora_translation=insert-delete-utf16-and-projection-rejection`.
5. The launched package and component are `ai.choosh` and
   `ai.choosh.MainActivity`; its content is attached, then teardown is observed.
6. The result bundle contains `package=ai.choosh`,
   `activity=ai.choosh.MainActivity`, `lifecycle=active-then-finished`, and
   `accessibility_label=Choosh`.
7. No instrumentation failure, crash, ANR, process death, or test failure appears
   in the Gradle XML/report output. Screenshots and logcat may diagnose failures
   but are never a pass oracle.

Archive the Gradle console log and
`android/app/build/reports/androidTests/connected/` as CI evidence. Record the
device API, ABI, and emulator/device identifier separately; those volatile values
must not alter the runner's stable result bundle.

This smoke proves that the published Sora widget and event API load and execute
on Android. It does **not** prove projection-feedback suppression, UTF-16 to UTF-8
range conversion, stale-revision handling, or idempotence. Those remain
deterministic JVM/Rust acceptance tests before M0-R3 can pass.
