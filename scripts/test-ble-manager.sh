#!/bin/sh
# Compiles the current manager against deterministic, hardware-free test adapters.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/offline-rust-toolchain.sh"
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
BLE_MANAGER_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
BLE_MANAGER_TMP=$(mktemp -d "${TMPDIR}/switch-ble-manager.XXXXXX")
trap 'rm -rf "$BLE_MANAGER_TMP"' EXIT INT TERM
"$BLE_MANAGER_ROOT/scripts/build-ble-core.sh"
ruby "$BLE_MANAGER_ROOT/app/Tests/BLEManager/prepare.rb" \
  "${BLE_MANAGER_SOURCE_DIR:-$BLE_MANAGER_ROOT/app/Sources}" "$BLE_MANAGER_TMP"
xcrun swiftc -parse-as-library \
  "$BLE_MANAGER_ROOT/app/Sources/AcceptanceContext.swift" \
  "$BLE_MANAGER_ROOT/app/Tests/BLEManager/Adapters.swift" \
  "$BLE_MANAGER_ROOT/app/Tests/BLEManager/main.swift" \
  "$BLE_MANAGER_TMP/BluetoothReaderManager.swift" \
  "$BLE_MANAGER_TMP/BLEPhaseDeadline.swift" \
  -Xlinker -force_load -Xlinker "$CARGO_TARGET_DIR/release/libproletarius_ble_core.a" \
  -o "$BLE_MANAGER_TMP/ble-manager-tests"
"$BLE_MANAGER_TMP/ble-manager-tests"
