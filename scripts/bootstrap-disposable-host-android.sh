#!/usr/bin/env bash
# Bootstrap the public half of the disposable Android-Keystore SSH identity.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
adb_bin="${ADB_BIN:-adb}"
serial="${ANDROID_SERIAL:-}"
apk="$root/android/app/build/outputs/apk/debug/app-debug.apk"
test_apk="$root/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"

for command in "$adb_bin"; do
  command -v "$command" >/dev/null || { echo 'android_bootstrap_adb_required' >&2; exit 69; }
done
[[ -f "$apk" && -f "$test_apk" ]] || {
  echo 'android_bootstrap_debug_apks_required' >&2
  exit 69
}

adb_args=()
[[ -n "$serial" ]] && adb_args=(-s "$serial")
"$adb_bin" "${adb_args[@]}" get-state >/dev/null 2>&1 || {
  echo 'android_bootstrap_device_unavailable' >&2
  exit 69
}
"$adb_bin" "${adb_args[@]}" install -r "$apk" >/dev/null
"$adb_bin" "${adb_args[@]}" install -r "$test_apk" >/dev/null

result="$($adb_bin "${adb_args[@]}" shell am instrument -w -r \
  -e choosh.mode key-bootstrap \
  ai.choosh.test/ai.choosh.SmokeInstrumentation 2>/dev/null)"
key_line="$(printf '%s\n' "$result" | sed -n 's/^INSTRUMENTATION_RESULT: fixture_authorized_key=//p' | tail -n 1)"
identity="$(printf '%s\n' "$result" | sed -n 's/^INSTRUMENTATION_RESULT: fixture_identity=//p' | tail -n 1)"
[[ "$identity" == 'android-keystore-rsa-public-only' ]] || {
  echo 'android_bootstrap_keystore_unavailable' >&2
  exit 70
}
[[ "$key_line" =~ ^ssh-rsa[[:space:]][A-Za-z0-9+/]+={0,2}$ ]] || {
  echo 'android_bootstrap_public_key_invalid' >&2
  exit 70
}
printf '%s\n' "$key_line"
