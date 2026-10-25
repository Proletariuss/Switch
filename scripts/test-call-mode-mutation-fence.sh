#!/bin/sh
set -eu
SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SOURCE_ROOT/scripts/workspace-env.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR}/switch-call-mode-fence.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$SOURCE_ROOT/app/Sources/APIClient.swift" \
  "$SOURCE_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$SOURCE_ROOT/app/Sources/CallModels.swift" \
  "$SOURCE_ROOT/app/Sources/CallModeStore.swift" \
  "$SOURCE_ROOT/app/Tests/CallModeMutationFence/main.swift" \
  -o "$TMP_ROOT/call-mode-fence-tests"
"$TMP_ROOT/call-mode-fence-tests"
