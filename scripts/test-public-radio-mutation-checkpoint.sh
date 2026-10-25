#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
CHECKPOINT_TMP=$(mktemp -d "${TMPDIR}/switch-radio-checkpoint.XXXXXX")
trap 'rm -rf "$CHECKPOINT_TMP"' EXIT INT TERM
xcrun swiftc -parse-as-library "$SWITCH_SOURCE_ROOT/app/Sources/APIClient.swift" "$SWITCH_SOURCE_ROOT/app/Sources/ESIMRefreshModels.swift" "$SWITCH_SOURCE_ROOT/app/Tests/PublicRadioMutationCheckpoint/main.swift" -o "$CHECKPOINT_TMP/tests"
"$CHECKPOINT_TMP/tests"
"$CHECKPOINT_TMP/tests" corrupt
"$CHECKPOINT_TMP/tests" save-failure
CHECKPOINT_SUITE="switch-radio-checkpoint-$(basename "$CHECKPOINT_TMP")"
"$CHECKPOINT_TMP/tests" disk-write "$CHECKPOINT_SUITE" "$CHECKPOINT_TMP/original.json"
"$CHECKPOINT_TMP/tests" disk-read "$CHECKPOINT_SUITE" "$CHECKPOINT_TMP/original.json"
