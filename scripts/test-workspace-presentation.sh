#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
PRESENTATION_TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
PRESENTATION_TEST_DIR=$(mktemp -d "${TMPDIR}/switch-presentation-tests.XXXXXX")
trap 'rm -rf "$PRESENTATION_TEST_DIR"' EXIT INT TERM
xcrun swiftc -parse-as-library \
  "$PRESENTATION_TEST_ROOT/app/Sources/WorkspaceViewport.swift" \
  "$PRESENTATION_TEST_ROOT/app/Sources/Views/GlassComponents.swift" \
  "$PRESENTATION_TEST_ROOT/app/Tests/WorkspacePresentation/main.swift" \
  -o "$PRESENTATION_TEST_DIR/presentation-tests"
"$PRESENTATION_TEST_DIR/presentation-tests" "$@"
