#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-network-recovery.XXXXXX")
trap 'rm -f "$TEST_DIR/network-recovery-tests"; rmdir "$TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library -swift-version 5 \
  "$SWITCH_SOURCE_ROOT/app/Sources/NetworkModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ModuleNetworkStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ModuleNetworkRecovery/Adapters.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ModuleNetworkRecovery/main.swift" \
  -o "$TEST_DIR/network-recovery-tests"
"$TEST_DIR/network-recovery-tests"
