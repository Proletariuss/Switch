#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
set -- $("$ROOT/scripts/version.sh")
VERSION=$1
BUILD_NUMBER=$2
BACKEND_VERSION=$(awk '
  /^\[workspace\.package\]$/ { in_workspace=1; next }
  /^\[/ { in_workspace=0 }
  in_workspace && /^version[[:space:]]*=/ {
    value=$0; sub(/^[^=]*=[[:space:]]*/, "", value); gsub(/[\"]/, "", value); print value; exit
  }
' "$ROOT/rust/Cargo.toml")
[ -n "$BACKEND_VERSION" ] || { echo "错误：无法读取 Rust backend workspace version。" >&2; exit 1; }

BUILD_ROOT=$(mktemp -d "${TMPDIR}/switch-release.XXXXXX")
STAGING_DIR=
ARCHIVE_TEMP=
ARCHIVE_MANIFEST_TEMP=
DIST_PUBLISHED=0
ARCHIVE_PUBLISHED=0
MANIFEST_ARCHIVE_PUBLISHED=0
COMPLETED=0
same_file() {
  [ -f "$1" ] && [ -f "$2" ] &&
    [ "$(stat -f '%d:%i' "$1")" = "$(stat -f '%d:%i' "$2")" ]
}
cleanup() {
  if [ "$COMPLETED" -eq 0 ]; then
    if [ "$MANIFEST_ARCHIVE_PUBLISHED" -eq 1 ] &&
       same_file "$ARCHIVE_MANIFEST" "$ARCHIVE_MANIFEST_TEMP"; then
      rm -f "$ARCHIVE_MANIFEST"
    fi
    if [ "$ARCHIVE_PUBLISHED" -eq 1 ] && same_file "$ARCHIVE_DMG" "$ARCHIVE_TEMP"; then
      rm -f "$ARCHIVE_DMG"
    fi
    if [ "$DIST_PUBLISHED" -eq 1 ] && same_file "$DMG" "$STAGED_DMG"; then
      rm -f "$DMG"
    fi
  fi
  [ -z "$ARCHIVE_TEMP" ] || rm -f "$ARCHIVE_TEMP"
  [ -z "$ARCHIVE_MANIFEST_TEMP" ] || rm -f "$ARCHIVE_MANIFEST_TEMP"
  [ -z "$STAGING_DIR" ] || rm -rf "$STAGING_DIR"
  rm -rf "$BUILD_ROOT"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

APP="$BUILD_ROOT/Build/Products/Release/Switch.app"
DMG="$SWITCH_DIST_DIR/Switch-$VERSION-macOS27.dmg"
ARCHIVE_DIR="$SWITCH_DIST_DIR/archive"
ARCHIVE_DMG="$ARCHIVE_DIR/Switch-$VERSION-macOS27.dmg"
ARCHIVE_MANIFEST="$ARCHIVE_DIR/Switch-$VERSION-macOS27-manifest.json"

for artifact_path in "$DMG" "$ARCHIVE_DMG" "$ARCHIVE_MANIFEST"; do
  if [ -e "$artifact_path" ] || [ -L "$artifact_path" ]; then
    echo "错误：发行文件已存在，拒绝覆盖：$artifact_path" >&2
    exit 1
  fi
done

cd "$ROOT"
RELEASE_ACCEPTANCE_SCOPE=product-final
RELEASE_EVIDENCE_SHA256=
RELEASE_APP_TREE_SHA256=
RELEASE_MAIN_EXECUTABLE_SHA256=
RELEASE_BACKEND_SHA256=
RELEASE_VERIFIED_DMG_SHA256=
ATTESTED_APP=
if [ -n "${SWITCH_SOFTWARE_RELEASE_EVIDENCE:-}" ]; then
  RELEASE_ACCEPTANCE_SCOPE=no-device-software-release
  "$ROOT/scripts/preflight-source.sh" --software-release-evidence "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
  RELEASE_EVIDENCE_SHA256=$(shasum -a 256 "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" | awk '{print $1}')
  ATTESTED_APP=$(python3 "$ROOT/scripts/check-no-device-software-acceptance.py" \
    verified-app "$SWITCH_SOFTWARE_RELEASE_EVIDENCE")
  set -- $(python3 "$ROOT/scripts/check-no-device-software-acceptance.py" \
    verified-bindings "$SWITCH_SOFTWARE_RELEASE_EVIDENCE")
  [ "$#" -eq 3 ] || { echo "错误：无法读取已验收 App 的发布绑定。" >&2; exit 1; }
  RELEASE_APP_TREE_SHA256=$1
  RELEASE_MAIN_EXECUTABLE_SHA256=$2
  RELEASE_BACKEND_SHA256=$3
else
  "$ROOT/scripts/preflight-source.sh" --final-release
  xcodegen generate --spec project.yml
fi
SOURCE_DIGEST=$(python3 "$ROOT/scripts/source-identity.py")
# Mandatory synthetic initialization semantics, once per release (not per package).
"$ROOT/scripts/test-release-initialization.sh"
[ "$(python3 "$ROOT/scripts/source-identity.py")" = "$SOURCE_DIGEST" ] || {
  echo "错误：初始化门禁期间源码发生变化，拒绝发行。" >&2
  exit 1
}

mkdir -p "$SWITCH_DIST_DIR"
STAGING_DIR=$(mktemp -d "$SWITCH_DIST_DIR/.Switch-$VERSION-release.XXXXXX")
STAGED_DMG="$STAGING_DIR/$(basename "$DMG")"
STAGED_MANIFEST="$STAGING_DIR/release-manifest.json"
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ]; then
  mkdir -p "$(dirname "$APP")"
  ditto "$ATTESTED_APP" "$APP"
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" \
    verify-app-copy "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" "$APP"
else
  xcodebuild -project Switch.xcodeproj -scheme Switch \
    -configuration Release -destination 'platform=macOS' \
    -derivedDataPath "$BUILD_ROOT" ARCHS=arm64 MACOSX_DEPLOYMENT_TARGET=27.0 \
    MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
    PRODUCT_BUNDLE_IDENTIFIER=com.proletarius.switch build
fi
if [ "$(python3 "$ROOT/scripts/source-identity.py")" != "$SOURCE_DIGEST" ]; then
  echo "错误：App 准备期间源码发生变化，拒绝继续发行。" >&2
  exit 1
fi
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ]; then
  [ "$(shasum -a 256 "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" | awk '{print $1}')" = "$RELEASE_EVIDENCE_SHA256" ] || {
    echo "错误：无设备软件验收凭证在构建期间发生变化。" >&2
    exit 1
  }
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" verify "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
fi
python3 "$ROOT/scripts/verify-app-identity.py" "$APP" "$VERSION" "$BUILD_NUMBER" "$BACKEND_VERSION" "$SOURCE_DIGEST"
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ]; then
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" \
    verify-app-copy "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" "$APP"
else
  "$ROOT/scripts/sign-release-app.sh" "$APP"
fi
"$ROOT/scripts/package-dmg.sh" "$APP" "$STAGED_DMG" "Switch"
if [ -n "${SWITCH_NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$STAGED_DMG" --keychain-profile "$SWITCH_NOTARY_PROFILE" --wait
  xcrun stapler staple "$STAGED_DMG"
  hdiutil verify "$STAGED_DMG"
fi
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ]; then
  [ "$(shasum -a 256 "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" | awk '{print $1}')" = "$RELEASE_EVIDENCE_SHA256" ] || {
    echo "错误：无设备软件验收凭证在打包期间发生变化。" >&2
    exit 1
  }
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" verify \
    "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" verify-app-copy \
    "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" "$APP"
  DMG_SHA_BEFORE_VERIFICATION=$(shasum -a 256 "$STAGED_DMG" | awk '{print $1}')
  python3 "$ROOT/scripts/check-no-device-software-acceptance.py" verify-dmg-app \
    "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" "$STAGED_DMG"
  RELEASE_VERIFIED_DMG_SHA256=$(shasum -a 256 "$STAGED_DMG" | awk '{print $1}')
  [ "$RELEASE_VERIFIED_DMG_SHA256" = "$DMG_SHA_BEFORE_VERIFICATION" ] || {
    echo "错误：DMG 在只读挂载核对期间发生变化。" >&2
    exit 1
  }
fi
SWITCH_EXPECTED_SOURCE_DIGEST="$SOURCE_DIGEST" \
SWITCH_RELEASE_ACCEPTANCE_SCOPE="$RELEASE_ACCEPTANCE_SCOPE" \
SWITCH_PRODUCT_ACCEPTANCE_COMPLETE="$([ "$RELEASE_ACCEPTANCE_SCOPE" = product-final ] && printf true || printf false)" \
SWITCH_RELEASE_EVIDENCE_SHA256="$RELEASE_EVIDENCE_SHA256" \
SWITCH_RELEASE_APP_TREE_SHA256="$RELEASE_APP_TREE_SHA256" \
SWITCH_RELEASE_MAIN_EXECUTABLE_SHA256="$RELEASE_MAIN_EXECUTABLE_SHA256" \
SWITCH_RELEASE_BACKEND_SHA256="$RELEASE_BACKEND_SHA256" \
SWITCH_RELEASE_VERIFIED_DMG_SHA256="$RELEASE_VERIFIED_DMG_SHA256" \
  "$ROOT/scripts/generate-release-manifest.sh" "$STAGED_MANIFEST" "$STAGED_DMG"
CURRENT_SOURCE_DIGEST=$(python3 "$ROOT/scripts/source-identity.py")
if [ "$CURRENT_SOURCE_DIGEST" != "$SOURCE_DIGEST" ]; then
  echo "错误：发行清单生成期间源码发生变化，拒绝发布产物。" >&2
  exit 1
fi
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ] &&
   [ "$(shasum -a 256 "$SWITCH_SOFTWARE_RELEASE_EVIDENCE" | awk '{print $1}')" != "$RELEASE_EVIDENCE_SHA256" ]; then
  echo "错误：无设备软件验收凭证在发行清单生成期间发生变化。" >&2
  exit 1
fi
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ] &&
   [ "$(shasum -a 256 "$STAGED_DMG" | awk '{print $1}')" != "$RELEASE_VERIFIED_DMG_SHA256" ]; then
  echo "错误：DMG 在验证与发布之间发生变化。" >&2
  exit 1
fi

mkdir -p "$ARCHIVE_DIR"
ARCHIVE_TEMP=$(mktemp "$ARCHIVE_DIR/.Switch-$VERSION-macOS27.dmg.XXXXXX")
cp -p "$STAGED_DMG" "$ARCHIVE_TEMP"
source_sha=$(shasum -a 256 "$STAGED_DMG" | awk '{print $1}')
archive_sha=$(shasum -a 256 "$ARCHIVE_TEMP" | awk '{print $1}')
if [ "$source_sha" != "$archive_sha" ]; then
  echo "错误：本地归档 SHA-256 校验失败：$ARCHIVE_DMG" >&2
  exit 1
fi
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ] &&
   [ "$source_sha" != "$RELEASE_VERIFIED_DMG_SHA256" ]; then
  echo "错误：待发布 DMG 不再是完成挂载核对的文件。" >&2
  exit 1
fi
ln -n "$STAGED_DMG" "$DMG"
DIST_PUBLISHED=1
ln -n "$ARCHIVE_TEMP" "$ARCHIVE_DMG"
ARCHIVE_PUBLISHED=1
if [ "$RELEASE_ACCEPTANCE_SCOPE" = no-device-software-release ] && {
     [ "$(shasum -a 256 "$DMG" | awk '{print $1}')" != "$RELEASE_VERIFIED_DMG_SHA256" ] ||
     [ "$(shasum -a 256 "$ARCHIVE_DMG" | awk '{print $1}')" != "$RELEASE_VERIFIED_DMG_SHA256" ];
   }; then
  echo "错误：DMG 发布副本与挂载核对摘要不一致。" >&2
  exit 1
fi
ARCHIVE_MANIFEST_TEMP=$(mktemp "$ARCHIVE_DIR/.Switch-$VERSION-macOS27-manifest.json.XXXXXX")
cp -p "$STAGED_MANIFEST" "$ARCHIVE_MANIFEST_TEMP"
if ! cmp -s "$STAGED_MANIFEST" "$ARCHIVE_MANIFEST_TEMP"; then
  echo "错误：本地发行清单归档字节校验失败：$ARCHIVE_MANIFEST" >&2
  exit 1
fi
ln -n "$ARCHIVE_MANIFEST_TEMP" "$ARCHIVE_MANIFEST"
MANIFEST_ARCHIVE_PUBLISHED=1
mv "$STAGED_MANIFEST" "$SWITCH_DIST_DIR/release-manifest.json"
COMPLETED=1
echo "发行完成：$DMG"
echo "本地归档：$ARCHIVE_DMG (SHA-256 $source_sha)"
