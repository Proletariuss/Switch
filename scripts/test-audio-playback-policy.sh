#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

"$SWITCH_SOURCE_ROOT/scripts/build-audio-core.sh"
TEST_DIR=$(mktemp -d "$SWITCH_BUILD_DIR/tmp/audio-playback-policy.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
ARCHIVE="$CARGO_TARGET_DIR/release/libproletarius_audio_core.a"
SWIFT_COMPILER=${SWIFT_COMPILER:-$(command -v swiftc)}
"$SWIFT_COMPILER" -parse-as-library -swift-version 5 \
  -module-cache-path "$SWIFT_MODULECACHE_PATH" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioPlaybackPolicy.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AudioRouteRecoveryTickets.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ReaderVoWiFiAudioBridge.swift" \
  "$SWITCH_SOURCE_ROOT/scripts/test-audio-playback-policy.swift" \
  -Xlinker -force_load -Xlinker "$ARCHIVE" \
  -Xlinker -framework -Xlinker SystemConfiguration \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
