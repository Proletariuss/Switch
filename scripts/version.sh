#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
VERSION=$(sed 's/[[:space:]]*$//' "$ROOT/VERSION")
BUILD_NUMBER=$(sed 's/[[:space:]]*$//' "$ROOT/BUILD_NUMBER")

case "$VERSION" in
  *.*) MAJOR=${VERSION%%.*}; MINOR=${VERSION#*.} ;;
  *) echo "错误：VERSION 必须为主版本.两位次版本：$VERSION" >&2; exit 1 ;;
esac
case "$MAJOR" in
  ''|*[!0-9]*|0[0-9]*) echo "错误：VERSION 主版本无效：$VERSION" >&2; exit 1 ;;
esac
case "$MINOR" in
  [0-9][0-9]) ;;
  *) echo "错误：VERSION 必须为主版本.两位次版本：$VERSION" >&2; exit 1 ;;
esac
case "$BUILD_NUMBER" in
  ''|*[!0-9]*|0*) echo "错误：BUILD_NUMBER 必须是无前导零的正整数" >&2; exit 1 ;;
esac

printf '%s %s\n' "$VERSION" "$BUILD_NUMBER"
