#!/usr/bin/env bash
# Runs the non-visual Android instrumentation lane against an already-booted AVD/device.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
adb wait-for-device

for _ in $(seq 1 60); do
  if [ "$(adb shell getprop sys.boot_completed | tr -d '\r')" = "1" ]; then
    exec "$root/gradlew" :app:connectedDebugAndroidTest --stacktrace
  fi
  sleep 1
done

echo "Android device did not finish booting within 60 seconds" >&2
exit 1
