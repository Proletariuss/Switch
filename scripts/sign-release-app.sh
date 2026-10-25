#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

if [ "$#" -ne 1 ]; then
  echo "usage: sign-release-app.sh APP_PATH" >&2
  exit 2
fi

APP=$1
[ -d "$APP" ] || { echo "错误：App 不存在：$APP" >&2; exit 1; }

IDENTITY=${SWITCH_SIGN_IDENTITY:-}
if [ -n "$IDENTITY" ]; then
  codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP"
else
  echo "提示：未设置 SWITCH_SIGN_IDENTITY，重新应用本机 ad-hoc 签名。"
  codesign --force --deep --timestamp=none --sign - "$APP"
fi
codesign --verify --deep --strict "$APP"
