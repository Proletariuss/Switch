#!/bin/sh
# Synthetic API chunks and private temporary files only; no App/backend/hardware.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
SMS_EXPORT_TMP=$(mktemp -d "${TMPDIR}/switch-export-tests.XXXXXX")
trap 'rm -rf "$SMS_EXPORT_TMP"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$SWITCH_SOURCE_ROOT/app/Sources/APIClient.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/SMSArchiveExport.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSArchiveExport/main.swift" \
  -o "$SMS_EXPORT_TMP/sms-export-tests"
"$SMS_EXPORT_TMP/sms-export-tests"
