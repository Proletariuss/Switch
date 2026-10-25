#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
case "${1:-}" in
  '') ;;
  --help|-h) echo 'usage: scripts/build.sh (builds ../dist/Switch.app; no disk image)'; exit 0 ;;
  *) echo 'usage: scripts/build.sh' >&2; exit 2 ;;
esac
cd "$SWITCH_SOURCE_ROOT"
set -- $(scripts/version.sh)
VERSION=$1
BUILD_NUMBER=$2
xcodegen generate --spec project.yml
scripts/preflight-source.sh
xcodebuild -project Switch.xcodeproj -scheme Switch \
  -configuration Release -destination 'platform=macOS' \
  -derivedDataPath "$SWITCH_BUILD_DIR/DerivedData" \
  ARCHS=arm64 MACOSX_DEPLOYMENT_TARGET=27.0 \
  MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
  PRODUCT_BUNDLE_IDENTIFIER=com.proletarius.switch DISPLAY_NAME=Switch build
APP="$SWITCH_BUILD_DIR/DerivedData/Build/Products/Release/Switch.app"
python3 "$SWITCH_SOURCE_ROOT/scripts/audit-release-bundle.py" "$APP"
STAGE=$(mktemp -d "$SWITCH_DIST_DIR/.switch-app.XXXXXX")
BACKUP=
cleanup() {
  if [ -n "$BACKUP" ] && [ -e "$BACKUP" ] && [ ! -e "$SWITCH_DIST_DIR/Switch.app" ]; then
    mv "$BACKUP" "$SWITCH_DIST_DIR/Switch.app"
  fi
  rm -rf "$STAGE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
(umask 000; cp -R -X "$APP" "$STAGE/Switch.app")
python3 "$SWITCH_SOURCE_ROOT/scripts/audit-release-bundle.py" "$STAGE/Switch.app"
if [ -L "$SWITCH_DIST_DIR/Switch.app" ]; then
  echo '错误：Switch.app 是符号链接，拒绝替换。' >&2; exit 1
fi
if [ -e "$SWITCH_DIST_DIR/Switch.app" ]; then
  BACKUP="$STAGE/previous.app"
  mv "$SWITCH_DIST_DIR/Switch.app" "$BACKUP"
fi
mv "$STAGE/Switch.app" "$SWITCH_DIST_DIR/Switch.app"
echo "构建完成：$SWITCH_DIST_DIR/Switch.app"
