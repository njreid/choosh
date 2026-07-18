#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source_fixture="$root/scripts/fixtures/release-discovery"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"$root/scripts/check-release-discovery.sh" "$source_fixture" |
  grep -Eq '^release_discovery_ok tag=v0\.2\.0 apk=choosh-0\.2\.0\.apk sha256=[0-9a-f]{64}$'

cp -R "$source_fixture" "$work/bad-checksum"
sed -i 's/^b4/00/' "$work/bad-checksum/choosh-0.2.0.sha256"
if "$root/scripts/check-release-discovery.sh" "$work/bad-checksum" >/dev/null 2>&1; then
  echo 'bad checksum fixture was accepted' >&2
  exit 1
fi

cp -R "$source_fixture" "$work/bad-signer"
jq '.apk = "choosh-0.1.0.apk"' \
  "$work/bad-signer/choosh-0.2.0.apk.signer.json" > "$work/signer.tmp"
mv "$work/signer.tmp" "$work/bad-signer/choosh-0.2.0.apk.signer.json"
if "$root/scripts/check-release-discovery.sh" "$work/bad-signer" >/dev/null 2>&1; then
  echo 'bad signer association fixture was accepted' >&2
  exit 1
fi

cp -R "$source_fixture" "$work/changed-signer"
jq '.certificate_sha256 = "2222222222222222222222222222222222222222222222222222222222222222"' \
  "$work/changed-signer/choosh-0.2.0.apk.signer.json" > "$work/signer.tmp"
mv "$work/signer.tmp" "$work/changed-signer/choosh-0.2.0.apk.signer.json"
if "$root/scripts/check-release-discovery.sh" "$work/changed-signer" >/dev/null 2>&1; then
  echo 'changed update signing identity fixture was accepted' >&2
  exit 1
fi

echo release_discovery_fixture_tests_ok
