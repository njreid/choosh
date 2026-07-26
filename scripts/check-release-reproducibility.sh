#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture=${1:-"$root/scripts/fixtures/release-discovery"}
manifest="$fixture/release-manifest.sha256"
fail() { echo "release_reproducibility_failed: $1" >&2; exit 1; }

[[ -d "$fixture" ]] || fail "missing fixture directory"
[[ -f "$manifest" ]] || fail "missing release-manifest.sha256"
[[ $(wc -c < "$manifest") -le 16384 ]] || fail "manifest exceeds 16 KiB"

mapfile -t files < <(find "$fixture" -maxdepth 1 -type f ! -name release-manifest.sha256 -printf '%f\n' | LC_ALL=C sort)
(( ${#files[@]} > 0 && ${#files[@]} <= 128 )) || fail "invalid artifact count"
for name in "${files[@]}"; do
  [[ "$name" != *$'\n'* && "$name" != /* && "$name" != *".."* ]] || fail "unsafe artifact name"
done
expected=$(mktemp)
trap 'rm -f "$expected"' EXIT
(cd "$fixture" && sha256sum "${files[@]}") > "$expected"
cmp -s "$expected" "$manifest" || fail "artifact digest manifest mismatch"
printf 'release_reproducibility_ok artifacts=%s manifest=%s\n' "${#files[@]}" "$(sha256sum "$manifest" | cut -d' ' -f1)"
