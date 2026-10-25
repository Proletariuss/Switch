#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
WINDOW_TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
WINDOW_TEST_DIR=$(mktemp -d "${TMPDIR}/switch-window-tests.XXXXXX")
trap 'rm -rf "$WINDOW_TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$WINDOW_TEST_ROOT/app/Sources/MainWindowCoordinator.swift" \
  "$WINDOW_TEST_ROOT/app/Tests/WindowLifecycle/main.swift" \
  -o "$WINDOW_TEST_DIR/window-tests"
"$WINDOW_TEST_DIR/window-tests"
