#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
HEALTH_TEST_DIR=$(mktemp -d "${TMPDIR}/switch-sms-health.XXXXXX")
trap 'rm -rf "$HEALTH_TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library -swift-version 5 \
  "$SWITCH_SOURCE_ROOT/app/Sources/SMSStoreHealth.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSStoreHealth/main.swift" \
  -o "$HEALTH_TEST_DIR/tests"
"$HEALTH_TEST_DIR/tests"
