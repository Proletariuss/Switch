#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
VIEWPORT_TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
VIEWPORT_TEST_DIR=$(mktemp -d "${TMPDIR}/switch-viewport-tests.XXXXXX")
trap 'rm -rf "$VIEWPORT_TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$VIEWPORT_TEST_ROOT/app/Sources/WorkspaceViewport.swift" \
  "$VIEWPORT_TEST_ROOT/app/Tests/WorkspaceViewport/main.swift" \
  -o "$VIEWPORT_TEST_DIR/viewport-tests"
"$VIEWPORT_TEST_DIR/viewport-tests"
