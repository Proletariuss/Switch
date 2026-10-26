#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "$SWITCH_BUILD_DIR/tmp/support-diagnostics.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
export SUPPORT_DIAGNOSTIC_TEST_ROOT="$TEST_DIR/runtime"
"${SWIFT_COMPILER:-$(command -v swiftc)}" -parse-as-library -swift-version 5 \
  -D SWITCH_BLE_DIAGNOSTIC_TESTING -module-cache-path "$SWIFT_MODULECACHE_PATH" \
  "$SWITCH_SOURCE_ROOT/app/Sources/BoundedBLEDiagnosticLog.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ClientDiagnostics.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/SupportBundleBuilder.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SupportDiagnostics/main.swift" \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
