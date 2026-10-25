#!/bin/sh
# Production Store/aggregate fallback + extracted View progress policy,
# exercised with a suspended in-memory API. Never touches the real app.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-vowifi-readiness.XXXXXX")
trap 'rm -f "$TEST_DIR/vowifi-readiness-tests" "$TEST_DIR/ProgressPolicy.swift"; rmdir "$TEST_DIR"' EXIT INT TERM
ruby "$TEST_ROOT/app/Tests/VoWiFiReadiness/prepare.rb" \
  "$TEST_ROOT/app/Sources/Views/HomeView.swift" \
  "$TEST_ROOT/app/Sources/DashboardStore.swift" \
  "$TEST_DIR/ProgressPolicy.swift"
xcrun swiftc -parse-as-library -swift-version 5 \
  "$TEST_ROOT/app/Sources/ReaderModels.swift" \
  "$TEST_ROOT/app/Sources/DeviceModels.swift" \
  "$TEST_ROOT/app/Sources/ESIMModels.swift" \
  "$TEST_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$TEST_ROOT/app/Sources/VoWiFiRequestCheckpoint.swift" \
  "$TEST_ROOT/app/Sources/ModuleVoWiFiStore.swift" \
  "$TEST_ROOT/app/Sources/ReaderRuntimeStore.swift" \
  "$TEST_ROOT/app/Sources/DeviceSnapshotStore.swift" \
  "$TEST_ROOT/app/Tests/VoWiFiReadiness/Adapters.swift" \
  "$TEST_ROOT/app/Tests/VoWiFiReadiness/main.swift" \
  "$TEST_ROOT/app/Tests/VoWiFiReadiness/OperationTests.swift" \
  "$TEST_ROOT/app/Tests/VoWiFiReadiness/DurableTests.swift" \
  "$TEST_DIR/ProgressPolicy.swift" \
  -o "$TEST_DIR/vowifi-readiness-tests"
"$TEST_DIR/vowifi-readiness-tests" "$@"
