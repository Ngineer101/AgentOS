#!/usr/bin/env bash
# Build the Rust FFI (crates/ffi) for iOS and package it for Xcode.
#
#   ios/build-rust.sh            # release build (default)
#   PROFILE=debug ios/build-rust.sh
#
# Produces:
#   ios/Frameworks/AgentHostFFI.xcframework   static libs for device + simulator
#   ios/Generated/agenthost_ffi.swift         Swift bindings (regenerated)
#   ios/Generated/agenthost_ffiFFI.h, module.modulemap
#
# Needs: macOS with Xcode (xcodebuild), rustup. Apple Silicon simulators only
# (aarch64-apple-ios-sim); add x86_64-apple-ios + lipo for Intel Macs.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
cd "$ROOT"

PROFILE="${PROFILE:-release}"
CRATE=agenthost-ffi
LIB=libagenthost_ffi.a
MODULE=agenthost_ffiFFI
TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim)
export IPHONEOS_DEPLOYMENT_TARGET=17.0

if [[ "$PROFILE" == "release" ]]; then
  PROFILE_FLAG=(--release)
  PROFILE_DIR=release
else
  PROFILE_FLAG=()
  PROFILE_DIR=debug
fi

if command -v rustup >/dev/null; then
  rustup target add "${TARGETS[@]}"
fi

# 1. Static libraries for device and simulator (staticlib only: no dylib for iOS).
for t in "${TARGETS[@]}"; do
  echo "==> cargo rustc $CRATE --target $t ($PROFILE_DIR)"
  cargo rustc -p "$CRATE" --lib --crate-type staticlib ${PROFILE_FLAG[@]+"${PROFILE_FLAG[@]}"} --target "$t"
done

# 2. Swift bindings, generated from a host build of the same crate
#    (UniFFI library mode reads the metadata embedded in the library).
echo "==> generating Swift bindings"
cargo build -p "$CRATE" --lib
case "$(uname -s)" in
  Darwin) HOSTLIB="target/debug/libagenthost_ffi.dylib" ;;
  *)      HOSTLIB="target/debug/libagenthost_ffi.so" ;;
esac
GEN="$HERE/Generated"
mkdir -p "$GEN"
cargo run -q -p "$CRATE" --features bindgen --bin uniffi-bindgen-swift -- \
  --swift-sources --headers --modulemap \
  --module-name "$MODULE" --modulemap-filename module.modulemap \
  "$HOSTLIB" "$GEN"

# 3. XCFramework (static library + headers + module map per slice).
if ! command -v xcodebuild >/dev/null; then
  echo "xcodebuild not found: skipping the XCFramework (bindings are in $GEN)" >&2
  exit 0
fi
echo "==> creating XCFramework"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/headers"
cp "$GEN/$MODULE.h" "$GEN/module.modulemap" "$WORK/headers/"
OUT="$HERE/Frameworks/AgentHostFFI.xcframework"
rm -rf "$OUT"
mkdir -p "$HERE/Frameworks"
ARGS=()
for t in "${TARGETS[@]}"; do
  ARGS+=(-library "target/$t/$PROFILE_DIR/$LIB" -headers "$WORK/headers")
done
xcodebuild -create-xcframework "${ARGS[@]}" -output "$OUT"
echo "==> done: $OUT"
echo "Next: cd ios && xcodegen generate && open AgentHost.xcodeproj"
