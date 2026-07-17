#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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

build_one() {
  local target="$1" abi="$2" linker="$3"
  local linker_key="CARGO_TARGET_${target^^}_LINKER"
  linker_key="${linker_key//-/_}"
  env "$linker_key=$toolchain/$linker" cargo build \
    --manifest-path "$root/Cargo.toml" \
    --locked --release --target "$target" -p choosh-android-bridge
  local library="$root/target/$target/release/libchoosh_android_bridge.so"
  for symbol in choosh_bridge_abi_version choosh_bridge_generation \
    choosh_bridge_request_begin choosh_bridge_request_cancel choosh_bridge_recreate; do
    nm -D --defined-only "$library" | awk '{print $3}' | grep -Fx -- "$symbol" >/dev/null
  done
  mkdir -p "$stage/$abi"
  cp -- "$library" "$stage/$abi/libchoosh_android_bridge.so"
}

build_one aarch64-linux-android arm64-v8a aarch64-linux-android26-clang
build_one x86_64-linux-android x86_64 x86_64-linux-android26-clang

destination="$root/android/app/src/main/jniLibs"
mkdir -p "$destination/arm64-v8a" "$destination/x86_64"
cp -- "$stage/arm64-v8a/libchoosh_android_bridge.so" "$destination/arm64-v8a/"
cp -- "$stage/x86_64/libchoosh_android_bridge.so" "$destination/x86_64/"
echo 'android_rust_build_passed'
