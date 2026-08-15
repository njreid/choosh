#!/usr/bin/env bash
# CHOOSH_ANDROID_RUST_FEATURES (optional): extra `choosh-android-bridge`
# Cargo features to build with, e.g. "dev-passkey" — see that crate's
# `dev_passkey.rs` module doc comment for why this must never be set for a
# release build. CHOOSH_ANDROID_RUST_DEST (optional): where the built
# `.so`s land, defaulting to `android/app/src/main/jniLibs` (packaged into
# every build type). A caller building with CHOOSH_ANDROID_RUST_FEATURES
# set MUST also point CHOOSH_ANDROID_RUST_DEST at `src/debug/jniLibs`
# instead — AGP's own source-set precedence (debug overrides main for an
# identically-named file) is what actually keeps a feature-enabled `.so`
# out of a release APK, not anything in this script itself.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rust_features="${CHOOSH_ANDROID_RUST_FEATURES:-}"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [[ -z "$sdk" ]]; then
  echo 'android_rust_missing_sdk: set ANDROID_HOME or ANDROID_SDK_ROOT' >&2
  exit 2
fi

ndk="${ANDROID_NDK_HOME:-}"
if [[ -z "$ndk" ]]; then
  if [[ ! -d "$sdk/ndk" ]]; then
    echo "android_rust_missing_ndk: install an NDK under $sdk/ndk or set ANDROID_NDK_HOME" >&2
    exit 2
  fi
  ndk="$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
fi

host_tag='linux-x86_64'
toolchain="$ndk/toolchains/llvm/prebuilt/$host_tag/bin"
if [[ ! -x "$toolchain/aarch64-linux-android26-clang" || ! -x "$toolchain/x86_64-linux-android26-clang" ]]; then
  echo "android_rust_invalid_ndk: missing API 26 clang wrappers in $toolchain" >&2
  exit 2
fi

for target in aarch64-linux-android x86_64-linux-android; do
  if ! rustup target list --installed | grep -Fx -- "$target" >/dev/null; then
    echo "android_rust_missing_target: rustup target add $target" >&2
    exit 2
  fi
done

stage="$(mktemp -d -t choosh-android-rust.XXXXXX)"
trap 'rm -rf -- "$stage"' EXIT INT TERM

# Reproducible-build fix, confirmed by a real two-checkout diff (building the
# same source from /home/njr/code/choosh and a second checkout at a different
# absolute path): two problems compounded to make `lib/*/libchoosh_android_bridge.so`
# byte-different (but behaviorally identical) between checkouts.
#   1. Cargo's build metadata hash for path dependencies (choosh-android-bridge
#      -> choosh-protocol etc., all in-workspace path deps) embeds their
#      absolute filesystem location, which leaked into panic/debug-info
#      strings. `--remap-path-prefix` on both the workspace root and the
#      Cargo registry/home neutralizes that.
#   2. Independently, the default release profile's parallel codegen
#      (multiple codegen units compiled concurrently) lays out generated
#      per-type string tables (e.g. serde's known-variant-name arrays) in an
#      order that depends on which codegen unit's compilation happens to
#      finish first -- deterministic for repeated builds of one checkout,
#      but not guaranteed to match a different checkout/environment where
#      unit contents hash differently. `-C codegen-units=1` removes the
#      multi-unit link-order question entirely for this small crate graph;
#      the cost (slower codegen, no cross-unit parallelism) is fine here
#      since this script already builds serially per ABI.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
build_rustflags="--remap-path-prefix=$root=/build/choosh --remap-path-prefix=$cargo_home=/build/cargo-home -C codegen-units=1"

build_one() {
  local target="$1" abi="$2" linker="$3"
  local linker_key="CARGO_TARGET_${target^^}_LINKER"
  local cc_key="CC_${target//-/_}"
  local ar_key="AR_${target//-/_}"
  linker_key="${linker_key//-/_}"
  local -a cargo_args=(build --manifest-path "$root/Cargo.toml" --locked --release --target "$target" -p choosh-android-bridge)
  if [[ -n "$rust_features" ]]; then
    cargo_args+=(--features "$rust_features")
  fi
  env "$linker_key=$toolchain/$linker" "$cc_key=$toolchain/$linker" "$ar_key=$toolchain/llvm-ar" \
    RUSTFLAGS="$build_rustflags" cargo "${cargo_args[@]}"
  local library="$root/target/$target/release/libchoosh_android_bridge.so"
  # ai.choosh.NativeBridge's relay-protocol JNI surface (docs/specs/android-native-runtime.md);
  # replaces the pre-relay choosh_bridge_* C-ABI symbols this check used to require.
  for symbol in Java_ai_choosh_NativeBridge_nativeInit Java_ai_choosh_NativeBridge_nativeConnect__JLjava_lang_String_2 \
    Java_ai_choosh_NativeBridge_nativeListDevhosts__J Java_ai_choosh_NativeBridge_nativeClose__J; do
    nm -D --defined-only "$library" | awk '{print $3}' | grep -F -- "$symbol" >/dev/null
  done
  # When a feature was explicitly requested, fail loudly if the symbol it's
  # supposed to add didn't actually land — a silently-not-taken feature flag
  # would otherwise look identical to a successful build.
  if [[ "$rust_features" == *dev-passkey* ]]; then
    nm -D --defined-only "$library" | awk '{print $3}' | grep -F -- Java_ai_choosh_NativeBridge_nativeDevPasskeyRegister >/dev/null \
      || { echo "android_rust_dev_passkey_symbol_missing: dev-passkey feature was requested but its JNI symbol is absent from $library" >&2; exit 1; }
  fi
  mkdir -p "$stage/$abi"
  cp -- "$library" "$stage/$abi/libchoosh_android_bridge.so"
}

build_one aarch64-linux-android arm64-v8a aarch64-linux-android26-clang
build_one x86_64-linux-android x86_64 x86_64-linux-android26-clang

destination="${CHOOSH_ANDROID_RUST_DEST:-$root/android/app/src/main/jniLibs}"
mkdir -p "$destination/arm64-v8a" "$destination/x86_64"
cp -- "$stage/arm64-v8a/libchoosh_android_bridge.so" "$destination/arm64-v8a/"
cp -- "$stage/x86_64/libchoosh_android_bridge.so" "$destination/x86_64/"
echo 'android_rust_build_passed'
