#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
"$SWITCH_SOURCE_ROOT/scripts/build-audio-core.sh"
TEST_DIR=$(mktemp -d "$SWITCH_BUILD_DIR/tmp/module-audio-pairing.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
"${SWIFT_COMPILER:-$(command -v swiftc)}" -parse-as-library -swift-version 5 \
  -module-cache-path "$SWIFT_MODULECACHE_PATH" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioIOProcRetirement.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioBridge.swift" \
  "$SWITCH_SOURCE_ROOT/scripts/test-module-audio-pairing.swift" \
  -Xlinker -force_load -Xlinker "$CARGO_TARGET_DIR/release/libproletarius_audio_core.a" \
  -Xlinker -framework -Xlinker SystemConfiguration \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
