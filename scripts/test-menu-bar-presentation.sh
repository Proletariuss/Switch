#!/bin/sh
# Targeted offline tests; only native presentation + in-memory service doubles.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
MENU_BAR_TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
MENU_BAR_TEST_TMP=$(mktemp -d "${TMPDIR}/switch-menu-bar.XXXXXX")
trap 'rm -rf "$MENU_BAR_TEST_TMP"' EXIT INT TERM
ruby "$MENU_BAR_TEST_ROOT/app/Tests/MenuBarPresentation/prepare.rb" \
  "$MENU_BAR_TEST_ROOT/app/Sources" "$MENU_BAR_TEST_TMP"
xcrun swiftc -parse-as-library ${MENU_BAR_TEST_SWIFT_FLAGS:-} \
  "$MENU_BAR_TEST_ROOT/app/Sources/WorkspaceViewport.swift" \
  "$MENU_BAR_TEST_ROOT/app/Sources/AppPreferences.swift" \
  "$MENU_BAR_TEST_ROOT/app/Sources/Views/GlassComponents.swift" \
  "$MENU_BAR_TEST_ROOT/app/Sources/Views/AppSection.swift" \
  "$MENU_BAR_TEST_ROOT/app/Sources/MenuBarStatusSnapshot.swift" \
  "$MENU_BAR_TEST_TMP/MenuBarViews.swift" \
  "$MENU_BAR_TEST_TMP/PresentationRenderer.swift" \
  "$MENU_BAR_TEST_ROOT/app/Tests/MenuBarPresentation/Adapters.swift" \
  "$MENU_BAR_TEST_ROOT/app/Tests/MenuBarPresentation/main.swift" \
  -o "$MENU_BAR_TEST_TMP/menu-bar-tests"
"$MENU_BAR_TEST_TMP/menu-bar-tests" "$@"
