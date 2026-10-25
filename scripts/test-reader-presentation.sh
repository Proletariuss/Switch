#!/bin/sh
# Production ReaderCardView rendering, with value models and in-memory services.
# Never invokes the app backend, BLE, PC/SC, notifications or network.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
READER_PRESENTATION_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
READER_PRESENTATION_TMP=$(mktemp -d "${TMPDIR}/switch-reader-presentation.XXXXXX")
trap 'rm -rf "$READER_PRESENTATION_TMP"' EXIT INT TERM
READER_PRESENTATION_OUTPUT=${1:-"$SWITCH_BUILD_DIR/reader-presentation"}
READER_PRESENTATION_APP="$READER_PRESENTATION_TMP/ReaderPresentation.app"
mkdir -p "$READER_PRESENTATION_APP/Contents/MacOS" "$READER_PRESENTATION_APP/Contents/Resources"
cp "$READER_PRESENTATION_ROOT/app/Tests/ReaderPresentation/Info.plist" "$READER_PRESENTATION_APP/Contents/Info.plist"
xcrun actool --compile "$READER_PRESENTATION_APP/Contents/Resources" --platform macosx \
  --minimum-deployment-target 15.0 "$READER_PRESENTATION_ROOT/app/Resources/Assets.xcassets"
ruby "$READER_PRESENTATION_ROOT/app/Tests/ReaderPresentation/prepare.rb" \
  "$READER_PRESENTATION_ROOT/app/Sources" "$READER_PRESENTATION_TMP" "${READER_PRESENTATION_VIEW_SOURCE:-}"
xcrun swiftc -parse-as-library \
  "$READER_PRESENTATION_ROOT/app/Sources/WorkspaceViewport.swift" \
  "$READER_PRESENTATION_ROOT/app/Sources/AppPreferences.swift" \
  "$READER_PRESENTATION_ROOT/app/Sources/Views/GlassComponents.swift" \
  "$READER_PRESENTATION_ROOT/app/Sources/CarrierDisplayMetadata.swift" \
  "$READER_PRESENTATION_ROOT/app/Sources/ESIMModels.swift" \
  "$READER_PRESENTATION_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$READER_PRESENTATION_TMP/ReaderValueModels.swift" \
  "$READER_PRESENTATION_TMP/ReaderCardView.swift" \
  "$READER_PRESENTATION_TMP/ReaderFactory.swift" \
  "$READER_PRESENTATION_ROOT/app/Tests/ReaderPresentation/Adapters.swift" \
  "$READER_PRESENTATION_ROOT/app/Tests/ReaderPresentation/main.swift" \
  -o "$READER_PRESENTATION_APP/Contents/MacOS/reader-presentation-tests"
"$READER_PRESENTATION_APP/Contents/MacOS/reader-presentation-tests" "$READER_PRESENTATION_OUTPUT"
