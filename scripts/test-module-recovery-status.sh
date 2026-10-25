#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-module-recovery.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library -swift-version 5 \
  "$SWITCH_SOURCE_ROOT/app/Sources/ModuleRecoveryStatus.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/Views/ModuleRecoveryStatusView.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ModuleRecoveryStatus/APIClientAdapter.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ModuleRecoveryStatus/UIAdapters.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ModuleRecoveryStatus/main.swift" \
  -o "$TEST_DIR/module-recovery-tests"
"$TEST_DIR/module-recovery-tests"
