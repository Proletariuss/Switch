#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-sms-attention.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library "$SWITCH_SOURCE_ROOT/app/Sources/AttentionStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSAttention/main.swift" -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
