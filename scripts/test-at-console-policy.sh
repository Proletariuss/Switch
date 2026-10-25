#!/bin/sh
# Pure DTO fixtures: no application, serial port, network or user data.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
AT_CONSOLE_TMP=$(mktemp -d "${TMPDIR}/switch-at-console.XXXXXX")
trap 'rm -rf "$AT_CONSOLE_TMP"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$SWITCH_SOURCE_ROOT/app/Sources/NetworkModels.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ATConsolePolicy/main.swift" \
  -o "$AT_CONSOLE_TMP/at-console-tests"
"$AT_CONSOLE_TMP/at-console-tests"
