#!/bin/sh
set -eu

# The hdiutil shim writes a deliberately incomplete text fixture, never a DMG.
if [ "$(basename "$0")" = hdiutil ]; then
  action=${1:-}
  printf '%s\n' "$action" >> "$MOCK_HDIUTIL_EVENTS"
  shift
  case "$action" in
    create)
      output=
      for argument do output=$argument; done
      printf 'incomplete-test-image\n' > "$output"
      [ "${MOCK_HDIUTIL_FAIL_CREATE:-0}" = 0 ]
      ;;
    verify)
      [ "${MOCK_HDIUTIL_FAIL_VERIFY:-0}" = 0 ] && [ -f "$1" ]
      ;;
    *) exit 2 ;;
  esac
  exit
fi

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
SWITCH_SOURCE_ROOT="$ROOT"
. "$ROOT/scripts/workspace-env.sh"
FIXTURE=$(mktemp -d "${TMPDIR:-/tmp}/switch-release-atomic-test.XXXXXX")
trap 'rm -rf "$FIXTURE"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$FIXTURE/bin" "$FIXTURE/output"
python3 "$ROOT/app/Tests/ReleaseInitialization/make_bundle_fixture.py" "$ROOT" "$FIXTURE/Switch.app" "$FIXTURE/tiny"
ln -s "$ROOT/scripts/test-release-atomic.sh" "$FIXTURE/bin/hdiutil"
OUTPUT="$FIXTURE/output/Switch-test-macOS27.dmg"
MOCK_HDIUTIL_EVENTS="$FIXTURE/events"
export MOCK_HDIUTIL_EVENTS

if PATH="$FIXTURE/bin:$PATH" MOCK_HDIUTIL_FAIL_CREATE=1 \
  sh "$ROOT/scripts/package-dmg.sh" "$FIXTURE/Switch.app" "$OUTPUT" Switch >/dev/null 2>&1; then
  echo '错误：hdiutil create 失败却被当作成功' >&2
  exit 1
fi
[ ! -e "$OUTPUT" ] && [ -z "$(find "$FIXTURE/output" -name '.switch-dmg.*' -print)" ] || exit 1

if PATH="$FIXTURE/bin:$PATH" MOCK_HDIUTIL_FAIL_VERIFY=1 \
  sh "$ROOT/scripts/package-dmg.sh" "$FIXTURE/Switch.app" "$OUTPUT" Switch >/dev/null 2>&1; then
  echo '错误：hdiutil verify 失败却被当作成功' >&2
  exit 1
fi
[ ! -e "$OUTPUT" ] && [ -z "$(find "$FIXTURE/output" -name '.switch-dmg.*' -print)" ] || exit 1

PATH="$FIXTURE/bin:$PATH" sh "$ROOT/scripts/package-dmg.sh" \
  "$FIXTURE/Switch.app" "$OUTPUT" Switch >/dev/null
grep -qx 'incomplete-test-image' "$OUTPUT"
sh "$ROOT/scripts/generate-release-manifest.sh" "$FIXTURE/release-manifest.json" "$OUTPUT" >/dev/null
VERSION=$(sh "$ROOT/scripts/version.sh" | awk '{print $1}')
grep -Fq "\"name\":\"Switch-$VERSION-macOS27.dmg\"" "$FIXTURE/release-manifest.json"
if PATH="$FIXTURE/bin:$PATH" sh "$ROOT/scripts/package-dmg.sh" \
  "$FIXTURE/Switch.app" "$OUTPUT" Switch >/dev/null 2>&1; then
  echo '错误：已有发行文件被覆盖' >&2
  exit 1
fi
grep -qx 'incomplete-test-image' "$OUTPUT"
[ "$(grep -c '^create$' "$MOCK_HDIUTIL_EVENTS")" -eq 3 ]
[ "$(grep -c '^verify$' "$MOCK_HDIUTIL_EVENTS")" -eq 2 ]
echo 'release atomic package tests passed (3 creates/2 verifies, tiny signed fixture and text image only)'
