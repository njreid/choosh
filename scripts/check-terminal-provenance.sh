#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
record="$root/docs/licenses/terminal-provenance.md"
decision="$root/docs/evidence/terminal-go-no-go.json"

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
jq -e '
  type == "object" and
  keys == ["decision", "device_conformance", "gates", "schema_version"] and
  .schema_version == 1 and
  (.decision == "blocked" or .decision == "go") and
  (.gates | type == "array" and length == 4) and
  ([.gates[].id] | sort == ["font-provenance", "libghostty-pin", "renderer-graph", "zelland-licence"]) and
  ([.gates[].id] | unique | length == 4) and
  all(.gates[];
    keys == ["evidence", "id", "next_action", "status"] and
    (.status == "blocked" or .status == "passed") and
    (.next_action | type == "string" and length > 0 and length <= 256) and
    (.evidence | type == "array" and length > 0 and
      all(.[]; type == "string" and test("^\\.\\./(licenses|evidence)/[A-Za-z0-9._/-]+$")))) and
  (.device_conformance | type == "object" and
    keys == ["scenarios", "status", "targets"] and
    (.status == "not-run" or .status == "failed" or .status == "passed") and
    ([.targets[].id] | sort == ["android-high-vulkan", "android-low-vulkan", "android-mid-vulkan", "android-x86_64-emulator"]) and
    ([.targets[].id] | unique | length == 4) and
    all(.targets[];
      keys == ["evidence", "id"] and
      (.evidence | type == "array" and
        all(.[]; type == "string" and test("^[A-Za-z0-9][A-Za-z0-9._/-]+$") and (contains("..") | not)))) and
    (.scenarios | sort == [
      "alternate-screen-mouse-and-bracketed-paste",
      "damage-only-rendering",
      "dimension-clamping",
      "gpu-device-loss",
      "gpu-init-visible-fallback",
      "rotation-and-screen-lock",
      "surface-loss",
      "sustained-output-input-latency",
      "wide-combining-and-bidi-text"
    ])) and
  (if any(.gates[]; .status == "blocked") or .device_conformance.status != "passed"
   then .decision == "blocked"
   else .decision == "go" and all(.device_conformance.targets[]; .evidence | length > 0)
   end)
' "$decision" >/dev/null || {
  echo "terminal go/no-go evidence is malformed or internally inconsistent" >&2
  exit 1
}

decision_state=$(jq -r '.decision' "$decision")

while IFS= read -r evidence; do
  test -s "$root/docs/evidence/$evidence" || {
    echo "terminal go/no-go evidence path is missing: $evidence" >&2
    exit 1
  }
done <<EOF
$(jq -r '.gates[].evidence[], .device_conformance.targets[].evidence[]' "$decision")
EOF

if test "$decision_state" = blocked && rg -n '\b(libghostty[-_]vt|wgpu|glyphon)\b' \
  "$root/Cargo.toml" "$root/Cargo.lock" "$root/rust"/*/Cargo.toml >/dev/null; then
  echo "terminal renderer dependency added while the provenance decision is blocked" >&2
  exit 1
fi

echo "Terminal provenance evidence checks passed; go/no-go decision: $decision_state."
