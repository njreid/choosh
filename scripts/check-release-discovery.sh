#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture=${1:-"$root/scripts/fixtures/release-discovery"}
metadata="$fixture/releases.json"

fail() {
  echo "release_discovery_failed: $1" >&2
  exit 1
}

command -v jq >/dev/null || fail "jq is required"
[[ -f "$metadata" ]] || fail "missing releases.json"
[[ $(wc -c < "$metadata") -le 65536 ]] || fail "metadata exceeds 64 KiB"
jq -e '
  type == "object" and keys == ["releases"] and
  (.releases | type == "array" and length > 0 and length <= 100) and
  all(.releases[];
    type == "object" and keys == ["assets", "draft", "prerelease", "tag"] and
    (.tag | type == "string" and length <= 64) and
    (.draft | type == "boolean") and (.prerelease | type == "boolean") and
    (.assets | type == "array" and length <= 32 and
      all(.[]; type == "string" and length <= 128 and
        test("^[A-Za-z0-9][A-Za-z0-9._-]*$"))))
' "$metadata" >/dev/null || fail "invalid bounded metadata shape"

mapfile -t stable_versions < <(
  jq -r '.releases[] | select(.draft == false and .prerelease == false) | .tag' "$metadata" |
    sed -nE 's/^v([0-9]+\.[0-9]+\.[0-9]+)$/\1/p' |
    sort -Vu
)
(( ${#stable_versions[@]} > 0 )) || fail "no stable semantic version"
version=${stable_versions[${#stable_versions[@]}-1]}
tag="v$version"
apk="choosh-$version.apk"
checksum="choosh-$version.sha256"
signer="choosh-$version.apk.signer.json"

release_count=$(jq --arg tag "$tag" '[.releases[] | select(.tag == $tag)] | length' "$metadata")
[[ "$release_count" == 1 ]] || fail "selected tag is not unique"
mapfile -t assets < <(jq -r --arg tag "$tag" '.releases[] | select(.tag == $tag) | .assets[]' "$metadata" | sort)
expected=("$apk" "$signer" "$checksum")
for expected_asset in "${expected[@]}"; do
  matches=0
  for asset in "${assets[@]}"; do
    [[ "$asset" == "$expected_asset" ]] && (( matches += 1 ))
  done
  [[ "$matches" == 1 ]] || fail "selected release asset $expected_asset is not unique"
done
apk_count=$(printf '%s\n' "${assets[@]}" | sed -nE '/^choosh-[0-9]+\.[0-9]+\.[0-9]+\.apk$/p' | wc -l)
[[ "$apk_count" == 1 ]] || fail "selected release has ambiguous installable APKs"

for asset in "$apk" "$checksum" "$signer"; do
  [[ -f "$fixture/$asset" ]] || fail "missing selected asset $asset"
done
[[ $(wc -c < "$fixture/$checksum") -le 256 ]] || fail "checksum evidence is oversized"
[[ $(wc -c < "$fixture/$signer") -le 4096 ]] || fail "signer evidence is oversized"
[[ $(wc -l < "$fixture/$checksum") == 1 ]] || fail "checksum list must contain one entry"
read -r recorded_digest recorded_name extra < "$fixture/$checksum"
[[ -z "${extra:-}" && "$recorded_name" == "$apk" && "$recorded_digest" =~ ^[0-9a-f]{64}$ ]] ||
  fail "checksum is not uniquely associated with selected APK"
actual_digest=$(sha256sum "$fixture/$apk" | cut -d ' ' -f 1)
[[ "$recorded_digest" == "$actual_digest" ]] || fail "selected APK checksum mismatch"

jq -e --arg apk "$apk" '
  type == "object" and keys == ["apk", "certificate_sha256", "schema_version"] and
  .schema_version == 1 and .apk == $apk and
  (.certificate_sha256 | type == "string" and test("^[0-9a-f]{64}$"))
' "$fixture/$signer" >/dev/null || fail "signer evidence is not associated with selected APK"

if (( ${#stable_versions[@]} > 1 )); then
  previous_version=${stable_versions[${#stable_versions[@]}-2]}
  previous_signer="$fixture/choosh-$previous_version.apk.signer.json"
  [[ -f "$previous_signer" && $(wc -c < "$previous_signer") -le 4096 ]] ||
    fail "previous stable signer evidence is missing or oversized"
  selected_certificate=$(jq -r '.certificate_sha256' "$fixture/$signer")
  previous_certificate=$(jq -r '.certificate_sha256' "$previous_signer")
  [[ "$selected_certificate" == "$previous_certificate" ]] ||
    fail "selected release changes the update signing identity"
fi

printf 'release_discovery_ok tag=%s apk=%s sha256=%s\n' "$tag" "$apk" "$actual_digest"
