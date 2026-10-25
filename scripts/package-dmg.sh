#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

if [ "$#" -ne 3 ]; then
  echo "usage: package-dmg.sh APP_PATH OUTPUT_DMG VOLUME_NAME" >&2
  exit 2
fi

APP_PATH="$1"
OUTPUT_DMG="$2"
VOLUME_NAME="$3"

[ -d "${APP_PATH}" ] || { echo "错误：App 不存在：${APP_PATH}" >&2; exit 1; }
if [ -e "${OUTPUT_DMG}" ] || [ -L "${OUTPUT_DMG}" ]; then
  echo "错误：DMG 已存在，拒绝覆盖：${OUTPUT_DMG}" >&2
  exit 1
fi

STAGE_DIR=$(mktemp -d "${TMPDIR}/switch-dmg.XXXXXX")
OUTPUT_STAGE=
cleanup() {
  rm -rf "${STAGE_DIR}"
  [ -z "${OUTPUT_STAGE}" ] || rm -rf "${OUTPUT_STAGE}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$(dirname "${OUTPUT_DMG}")"
OUTPUT_STAGE=$(mktemp -d "$(dirname "${OUTPUT_DMG}")/.switch-dmg.XXXXXX")
STAGED_DMG="${OUTPUT_STAGE}/$(basename "${OUTPUT_DMG}")"

python3 "${SWITCH_SOURCE_ROOT}/scripts/audit-release-bundle.py" "${APP_PATH}"
# Darwin ditto may preserve file xattrs despite noextattr; both audits remain mandatory.
(umask 000; cp -R -X "${APP_PATH}" "${STAGE_DIR}/Switch.app")
ln -s /Applications "${STAGE_DIR}/Applications"
python3 "${SWITCH_SOURCE_ROOT}/scripts/audit-release-bundle.py" "${STAGE_DIR}/Switch.app" --stage-root "${STAGE_DIR}"

hdiutil create \
  -volname "${VOLUME_NAME}" \
  -srcfolder "${STAGE_DIR}" \
  -format UDZO \
  -imagekey zlib-level=9 \
  "${STAGED_DMG}"
hdiutil verify "${STAGED_DMG}"
ln -n "${STAGED_DMG}" "${OUTPUT_DMG}"
