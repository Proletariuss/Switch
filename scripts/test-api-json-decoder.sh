#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUTPUT_DIR=${1:-"$SWITCH_BUILD_DIR/optimization/2026-09-26-sms-decoder"}
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR=$(CDPATH= cd -- "$OUTPUT_DIR" && pwd)

{
  date -u '+%Y-%m-%dT%H:%M:%SZ'
  xcrun swiftc --version
  sw_vers
  uname -m
  sysctl -n machdep.cpu.brand_string
  git -C "$ROOT" rev-parse HEAD
  shasum -a 256 "$ROOT/app/Sources/APIClient.swift" "$ROOT/app/Sources/SMSModels.swift"
  printf '%s\n' 'Flags: -O -whole-module-optimization -swift-version 5'
  printf '%s\n' 'Offline synthetic 0/100/1000 SMS; 6 warmups + 40 measured samples per operation.'
} | tee "$OUTPUT_DIR/environment.txt"

xcrun swiftc -O -whole-module-optimization -swift-version 5 \
  "$ROOT/app/Sources/APIClient.swift" \
  "$ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$ROOT/app/Sources/SMSModels.swift" \
  "$ROOT/app/Tests/APIJSONDecoder/BaselineDecoder.swift" \
  "$ROOT/app/Tests/APIJSONDecoder/Benchmark.swift" \
  -o "$OUTPUT_DIR/api-json-decoder-bench"
"$OUTPUT_DIR/api-json-decoder-bench" "$OUTPUT_DIR/results.json" > "$OUTPUT_DIR/results.txt"
sed -n '1,80p' "$OUTPUT_DIR/results.txt"
