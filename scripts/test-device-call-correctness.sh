#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-device-call.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
ruby "$SWITCH_SOURCE_ROOT/app/Tests/DeviceCallCorrectness/prepare.rb" "$SWITCH_SOURCE_ROOT/app/Sources" "$TEST_DIR"
xcrun swiftc -parse-as-library -swift-version 5 \
  "$SWITCH_SOURCE_ROOT/app/Sources/CallModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/CallRuntimeStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/NetworkServicesStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ModuleNetworkStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/NetworkModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/SafeDiagnosticSummary.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/DeviceCallCorrectness/Adapters.swift" \
  "$TEST_DIR/ExtractedPolicies.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/DeviceCallCorrectness/main.swift" \
  -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
