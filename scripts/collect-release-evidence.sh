#!/usr/bin/env bash
# Collects a signed release APK plus its SBOM, licence notices, checksum, and
# signer evidence into build/release/, named for Obtainium's GitHub Releases
# discovery (docs/release-android.md) and scripts/check-release-discovery.sh's
# exact expectations. Shared by .github/workflows/release-android.yml and
# `just release-bundle` so CI and a local run produce identical evidence.
#
# Usage: collect-release-evidence.sh VERSION
# Requires: android/app/build/outputs/apk/release/*.apk,
# android/app/build/reports/{bom.json,licenses/NOTICE.txt} already built
# (`./gradlew :app:assembleRelease :app:cyclonedxBom :app:generateReleaseLicenseReport`).
set -euo pipefail

version="${1:?usage: collect-release-evidence.sh VERSION}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release_dir="${RELEASE_DIR:-$root/build/release}"
apksigner="${APKSIGNER:-}"

if [[ -z "$apksigner" ]]; then
  build_tools="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}/build-tools/36.0.0"
  apksigner="$build_tools/apksigner"
fi
if [[ ! -x "$apksigner" ]]; then
  echo "collect_release_evidence_missing_apksigner: set APKSIGNER or install \$ANDROID_HOME/build-tools/36.0.0/apksigner" >&2
  exit 2
fi

mapfile -t apks < <(find "$root/android/app/build/outputs/apk/release" -maxdepth 1 -type f -name '*.apk' -print)
if (( ${#apks[@]} != 1 )); then
  echo "collect_release_evidence_ambiguous_apk: expected exactly one universal release APK, found ${#apks[@]}" >&2
  exit 1
fi

mkdir -p "$release_dir"
apk="$release_dir/choosh-${version}.apk"
cp -- "${apks[0]}" "$apk"

verify_output="$("$apksigner" verify --verbose --print-certs "$apk")"
echo "$verify_output"
cert_sha256="$(printf '%s\n' "$verify_output" | sed -nE 's/^Signer #1 certificate SHA-256 digest: ([0-9a-f]{64})$/\1/p')"
if [[ ! "$cert_sha256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "collect_release_evidence_unparsed_signer: could not parse a signer certificate SHA-256 digest from apksigner output" >&2
  exit 1
fi

cp -- "$root/android/app/build/reports/bom.json" "$release_dir/choosh-${version}.cdx.json"
cp -- "$root/android/app/build/reports/licenses/NOTICE.txt" "$release_dir/choosh-${version}-NOTICE.txt"

# scripts/check-release-discovery.sh requires exactly one checksum line,
# uniquely associated with the selected APK — not a combined hash of every
# release asset — so this must name only the APK, not "$release_dir"/*.
(cd "$release_dir" && sha256sum "choosh-${version}.apk") > "$release_dir/choosh-${version}.sha256"

# scripts/check-release-discovery.sh also requires a signer-evidence asset
# naming this exact APK and its certificate digest, to detect a silent
# signing-key change between releases (Obtainium refuses to update across one).
printf '{"schema_version": 1, "apk": "choosh-%s.apk", "certificate_sha256": "%s"}\n' \
  "$version" "$cert_sha256" > "$release_dir/choosh-${version}.apk.signer.json"

echo "collect_release_evidence_ok version=$version dir=$release_dir"
ls -la "$release_dir"
