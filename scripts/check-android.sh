#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
catalog="$root/gradle/libs.versions.toml"
manifest="$root/android/app/src/main/AndroidManifest.xml"

test -s "$root/gradle/wrapper/gradle-wrapper.jar"
test -s "$catalog"
test -s "$manifest"

if grep -En '= *"(latest|[^" ]*[+*][^" ]*)"' "$catalog"; then
  echo "dynamic Gradle version is forbidden" >&2
  exit 1
fi

grep -Fq 'namespace = "ai.choosh"' "$root/android/app/build.gradle.kts"
grep -Fq 'applicationId = "ai.choosh"' "$root/android/app/build.gradle.kts"
grep -Fq 'compileSdk = 36' "$root/android/app/build.gradle.kts"
grep -Fq 'targetSdk = 36' "$root/android/app/build.gradle.kts"
grep -Fq 'distributionUrl=https\://services.gradle.org/distributions/gradle-9.6.1-bin.zip' \
  "$root/gradle/wrapper/gradle-wrapper.properties"
grep -Fq 'distributionSha256Sum=9c0f7faeeb306cb14e4279a3e084ca6b596894089a0638e68a07c945a32c9e14' \
  "$root/gradle/wrapper/gradle-wrapper.properties"
test "$(sha256sum "$root/gradle/wrapper/gradle-wrapper.jar" | cut -d ' ' -f 1)" = \
  '497c8c2a7e5031f6aa847f88104aa80a93532ec32ee17bdb8d1d2f67a194a9c7'

# SSH to a user-configured host requires INTERNET. Every other permission is a
# reviewed addition, so the declared set must stay exactly this one.
grep -Fq '<uses-permission android:name="android.permission.INTERNET" />' "$manifest"
test "$(grep -c 'uses-permission' "$manifest")" = 1

echo "Android static policy checks passed. Device and emulator behavior was not exercised."
