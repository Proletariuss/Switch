#!/bin/sh
# Hardware-free tests of the production eSIM Store and actual View bindings.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-reader-esim.XXXXXX")
trap 'rm -f "$TEST_DIR/reader-esim-tests" "$TEST_DIR/ReaderCardViewBinding.swift"; rmdir "$TEST_DIR"' EXIT INT TERM
ruby "$TEST_ROOT/app/Tests/ReaderESIMIdentity/prepare.rb" \
  "$TEST_ROOT/app/Sources/Views/ReaderCardView.swift" "$TEST_DIR/ReaderCardViewBinding.swift"
xcrun swiftc -parse-as-library \
  "$TEST_ROOT/app/Sources/ESIMModels.swift" \
  "$TEST_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$TEST_ROOT/app/Sources/ReaderESIMStore.swift" \
  "$TEST_ROOT/app/Sources/ModuleESIMStore.swift" \
  "$TEST_ROOT/app/Tests/ReaderESIMIdentity/Adapters.swift" \
  "$TEST_ROOT/app/Tests/ReaderESIMIdentity/main.swift" \
  "$TEST_DIR/ReaderCardViewBinding.swift" \
  -o "$TEST_DIR/reader-esim-tests"
"$TEST_DIR/reader-esim-tests"
