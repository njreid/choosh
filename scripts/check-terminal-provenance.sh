#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
record="$root/docs/licenses/terminal-provenance.md"

check_hash() {
  expected=$1
  path=$2
  actual=$(sha256sum "$root/$path" | cut -d ' ' -f 1)
  test "$actual" = "$expected" || {
    echo "terminal provenance hash mismatch: $path" >&2
    exit 1
  }
}

check_hash baf3fa2b1078c6a5cac05196889c01d63536ed6233e705262c7e6d4fbefffa59 android/app/src/main/res/font/geomini.ttf
check_hash ae87c9bc7baae0a18e78cbe498d967865c251cae20fffa0c34e5937ce118f845 android/app/src/main/res/font/iosevka_charon_mono.ttf
check_hash d5a0e6259a77a98b086897b3b86f120c1170b85ab5e82f527cf810e239f082cf android/app/src/main/res/font/iosevka_charon_mono_bold.ttf
check_hash f540e48ef1971065cb9ec32f31a4dc83c1bef7be9e34ed6883a8284fa942aec0 android/app/src/main/res/raw/geomini_ofl.txt
check_hash 58b40bf4152bcb93ecc20489aad21093b5b1e67d64e6814e7f1cb6615cf50784 android/app/src/main/res/raw/iosevka_charon_mono_ofl.txt

grep -Fq 'SIL OPEN FONT LICENSE Version 1.1' "$root/android/app/src/main/res/raw/geomini_ofl.txt"
grep -Fq 'SIL OPEN FONT LICENSE Version 1.1' "$root/android/app/src/main/res/raw/iosevka_charon_mono_ofl.txt"

for component in Zelland libghostty-vt wgpu glyphon 'Iosevka Charon Mono' Geomini; do
  grep -Fq "$component" "$record" || {
    echo "terminal provenance record missing component: $component" >&2
    exit 1
  }
done
grep -Fq 'Status: **blocked**' "$record"

if rg -n '\b(libghostty[-_]vt|wgpu|glyphon)\b' \
  "$root/Cargo.toml" "$root/Cargo.lock" "$root/rust"/*/Cargo.toml >/dev/null; then
  echo "terminal renderer dependency added before provenance record was cleared" >&2
  exit 1
fi

echo "Terminal provenance evidence checks passed; renderer import remains blocked."
