#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUT=$(mktemp "${TMPDIR:-/tmp}/switch-reader-vowifi-input.XXXXXX")
trap 'rm -f "$OUT"' EXIT INT TERM
xcrun swiftc -parse-as-library -swift-version 5 \
  "$ROOT/app/Sources/DeviceModels.swift" \
  "$ROOT/app/Sources/ESIMModels.swift" \
  "$ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$ROOT/app/Sources/ReaderModels.swift" \
  "$ROOT/app/Tests/ReaderVoWiFiInputBoundary/main.swift" \
  -o "$OUT"
"$OUT"
