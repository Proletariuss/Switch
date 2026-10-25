#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
BLE_MANAGER_TEST_FILTER="GATT characteristic selection differential golden" \
  "$ROOT/scripts/test-ble-manager.sh"
