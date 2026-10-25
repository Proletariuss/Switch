#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
"$SWITCH_SOURCE_ROOT/scripts/build-audio-core.sh"
SWITCH_BUILD_ROOT="$SWITCH_BUILD_DIR"
TEST_CACHE="$SWITCH_BUILD_ROOT/swift-cache/call-audio-store"
mkdir -p "$SWITCH_BUILD_ROOT/tmp" "$TEST_CACHE/clang"
TEST_DIR=$(mktemp -d "$SWITCH_BUILD_ROOT/tmp/call-audio-store.XXXXXX")
# Remove only this invocation's temporary harness, never the shared caches.
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
export TMPDIR="$SWITCH_BUILD_ROOT/tmp/"
export CLANG_MODULE_CACHE_PATH="$TEST_CACHE/clang"
export SWIFT_MODULECACHE_PATH="$TEST_CACHE"
export CALL_AUDIO_STORE_TEST_ROOT="$TEST_DIR"
SWIFT_COMPILER=${SWIFT_COMPILER:-$(command -v swiftc)}
"$SWIFT_COMPILER" -parse-as-library -swift-version 5 \
  -module-cache-path "$TEST_CACHE" \
  "$SWITCH_SOURCE_ROOT/app/Sources/CallModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioIOProcRetirement.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioRouteRecoveryTickets.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ReaderAudioAdmission.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/CallAudioStore.swift" \
  "$SWITCH_SOURCE_ROOT/scripts/test-call-audio-store.swift" \
  -Xlinker -force_load -Xlinker "$CARGO_TARGET_DIR/release/libproletarius_audio_core.a" \
  -Xlinker -framework -Xlinker SystemConfiguration \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
