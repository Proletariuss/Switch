#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-safe-events.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library -swift-version 5 \
  "$SWITCH_SOURCE_ROOT/app/Sources/DiagnosticEventModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/DiagnosticEventStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/DiagnosticEvents/main.swift" \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
