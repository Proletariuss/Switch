#!/bin/sh
# Production completion ledger and Unix server; no CoreBluetooth or real cards.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
"$SWITCH_SOURCE_ROOT/scripts/build-ble-core.sh"
BLE_PROOF_TMP=$(mktemp -d "${TMPDIR}/ble-proof.XXXXXX")
trap 'rm -rf "$BLE_PROOF_TMP"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$SWITCH_SOURCE_ROOT/app/Sources/ReaderBLEOperationLedger.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/ReaderBLEBridge.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ReaderBLEOperationProof/Adapters.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ReaderBLEOperationProof/main.swift" \
  -Xlinker -force_load -Xlinker "$CARGO_TARGET_DIR/release/libproletarius_ble_core.a" \
  -o "$BLE_PROOF_TMP/proof-tests"
"$BLE_PROOF_TMP/proof-tests" "$BLE_PROOF_TMP/bridge.sock"
