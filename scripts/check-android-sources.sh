#!/usr/bin/env bash
# Compiles the Android Java sources against the pinned platform SDK.
#
# This is a fast fail-closed guard, not a replacement for the Gradle build: it
# exists because a source that does not compile can otherwise reach `main` when
# Gradle cannot resolve AGP locally. Files needing a resolved library
# dependency (Sora) or generated resources are compiled against generated
# stubs, so this checks Java-level API use only.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
[[ -n "$sdk" ]] || { echo 'android_sources_sdk_root_required' >&2; exit 69; }
platform="$sdk/platforms/android-36/android.jar"
[[ -f "$platform" ]] || { echo 'android_sources_platform_36_required' >&2; exit 69; }
command -v javac >/dev/null || { echo 'android_sources_javac_required' >&2; exit 69; }

# JUnit is a resolved Gradle dependency. When it has not been fetched yet the
# main sources are still checked; the Gradle build remains authoritative for the
# unit-test source set.
cache="${GRADLE_USER_HOME:-$HOME/.gradle}/caches"
junit="$( { find "$cache" -name 'junit-4.13.2.jar' 2>/dev/null || true; } | head -1)"
hamcrest="$( { find "$cache" -name 'hamcrest-core-1.3.jar' 2>/dev/null || true; } | head -1)"
test_scope='android/app/src/test/java'
if [[ -z "$junit" || -z "$hamcrest" ]]; then
  junit='' hamcrest='' test_scope=''
fi

work="$(mktemp -d -t choosh-android-sources.XXXXXX)"
trap 'rm -rf -- "$work"' EXIT INT TERM
mkdir -p "$work/gen/ai/choosh" "$work/classes"

# AGP generates R from res/; stub exactly the identifiers the sources reference.
python3 - "$root" "$work/gen/ai/choosh/R.java" <<'PY'
import pathlib, re, sys
root, target = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
found = set()
for source in (root / "android/app/src/main/java").rglob("*.java"):
    found.update(re.findall(r"R\.(string|font|drawable|style|xml|raw)\.([A-Za-z0-9_]+)", source.read_text()))
groups: dict[str, list[str]] = {}
for kind, name in sorted(found):
    groups.setdefault(kind, []).append(name)
body = "\n".join(
    "  public static final class %s { %s }" % (kind, " ".join(
        "public static final int %s = %d;" % (name, index + 1) for index, name in enumerate(names)))
    for kind, names in sorted(groups.items()))
target.write_text("package ai.choosh;\npublic final class R {\n%s\n}\n" % body)
PY

# Sora is a resolved Gradle dependency, so its consumer is checked by the Gradle build only.
scopes=("$root/android/app/src/main/java")
[[ -n "$test_scope" ]] && scopes+=("$root/$test_scope")
mapfile -t sources < <(find "${scopes[@]}" -name '*.java' ! -name 'SoraContentChangeTranslator.java' | sort)

javac -nowarn -classpath "$platform${junit:+:$junit}${hamcrest:+:$hamcrest}" -d "$work/classes" \
  "${sources[@]}" "$work/gen/ai/choosh/R.java"

if [[ -n "$test_scope" ]]; then
  echo 'android_sources_compile_ok scope=main+test'
else
  echo 'android_sources_compile_ok scope=main junit=unresolved'
fi
