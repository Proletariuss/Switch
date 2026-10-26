#!/bin/sh
# Compile only the production state machine and fake API fixture. Never the
# live adapter, application, card or notification client.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
DEVICE_SMS_TMP=$(mktemp -d "${TMPDIR}/switch-device-sms-send.XXXXXX")
trap 'rm -rf "$DEVICE_SMS_TMP"' EXIT INT TERM
xcrun swiftc -parse-as-library -D SWITCH_DEVICE_SMS_TESTS \
  "$SWITCH_SOURCE_ROOT/app/Sources/DeviceSMSIssuance.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/DeviceSMSSendStore.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/DeviceSMSSend/main.swift" \
  -o "$DEVICE_SMS_TMP/device-sms-send-tests"
"$DEVICE_SMS_TMP/device-sms-send-tests"
