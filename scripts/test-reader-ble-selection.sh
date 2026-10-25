#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-reader-ble-selection.XXXXXX")
trap 'rm -f "$TEST_DIR/ReaderBLEManualSelection.swift" "$TEST_DIR/reader-ble-selection-tests"; rmdir "$TEST_DIR"' EXIT INT TERM
ruby "$TEST_ROOT/app/Tests/ReaderBLESelection/prepare.rb" \
  "$TEST_ROOT/app/Sources/Views/ReaderCardView.swift" "$TEST_DIR/ReaderBLEManualSelection.swift"
xcrun swiftc -parse-as-library \
  "$TEST_DIR/ReaderBLEManualSelection.swift" \
  "$TEST_ROOT/app/Tests/ReaderBLESelection/main.swift" \
  -o "$TEST_DIR/reader-ble-selection-tests"
"$TEST_DIR/reader-ble-selection-tests"
