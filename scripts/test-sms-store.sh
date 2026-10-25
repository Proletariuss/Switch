#!/bin/sh
# Deterministic tests of production refresh/deletion code; no backend, reader,
# user defaults, notification permissions or user data are touched.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
SMS_STORE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
SMS_STORE_TMP=$(mktemp -d "${TMPDIR}/switch-sms-store.XXXXXX")
trap 'rm -rf "$SMS_STORE_TMP"' EXIT INT TERM
ruby "$SMS_STORE_ROOT/app/Tests/SMSStore/prepare.rb" \
  "${SMS_STORE_SOURCE_DIR:-$SMS_STORE_ROOT/app/Sources}" "$SMS_STORE_TMP"
xcrun swiftc -parse-as-library \
  "$SMS_STORE_ROOT/app/Tests/SMSStore/Adapters.swift" \
  "$SMS_STORE_ROOT/app/Tests/SMSStore/main.swift" \
  "$SMS_STORE_TMP/SMSStore.swift" "$SMS_STORE_TMP/SMSModels.swift" \
  -o "$SMS_STORE_TMP/sms-store-tests"
"$SMS_STORE_TMP/sms-store-tests"
